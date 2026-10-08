//! `rdy worktree add|ls|rm` (wcwt1, FW-V2): extra human checkouts of one project, each bound to
//! its own view. There is no agent form. Every other working-copy command run inside a worktree
//! uses its place (`Place::locate` reads the pointer file).

use super::{Open, checkout, open_at, tree};
use crate::Env;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::files::{dirty_paths, remove_tracked};
use rdy_engine::wc::state::{
    POINTER_FILE, Place, WORKTREE_DIR_FILE, WORKTREES_DIR, WorktreeInfo, worktrees,
};
use rdy_engine::{E_BAD_INPUT, E_IO, E_WC_DIRTY, RdyError};
use serde_json::{Value, json};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum WorktreeCmd {
    /// Check a view out into a new directory (absent or empty) as an extra working copy.
    Add {
        /// The new worktree's directory.
        dir: PathBuf,
        /// The view to bind it to.
        view: String,
    },
    /// List the worktrees.
    Ls,
    /// Remove a worktree's tracked files and state. The view is never touched.
    Rm {
        /// The worktree's directory.
        dir: PathBuf,
        /// Remove it although it holds uncommitted edits.
        #[arg(long)]
        force: bool,
    },
}

pub(super) fn run(env: &Env, cmd: WorktreeCmd) -> Result<(Value, String), ErrorReply> {
    match cmd {
        WorktreeCmd::Add { dir, view } => add(env, &dir, view),
        WorktreeCmd::Ls => ls(env),
        WorktreeCmd::Rm { dir, force } => rm(env, &dir, force),
    }
}

fn io_err(path: &Path, what: &str, e: impl std::fmt::Display) -> ErrorReply {
    RdyError::new(
        E_IO,
        format!("cannot {what} {}: {e}", path.display()),
        "check the path and its permissions",
    )
    .into()
}

fn absolute(p: &Path) -> Result<PathBuf, ErrorReply> {
    std::path::absolute(p).map_err(|e| io_err(p, "resolve", e))
}

/// The directory's last component with every character outside `[A-Za-z0-9._-]` replaced by `-`;
/// `worktree` when nothing usable is left.
fn sanitized_name(dir: &Path) -> String {
    let raw = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "worktree".to_string()
    } else {
        name
    }
}

/// Creates `.rdy/worktrees/<name>` for the first free name (`name`, `name-2`, `name-3`, …) and
/// returns the name. The creation itself reserves the name, so two adds at once get two names.
fn reserve(project: &Path, base: &str) -> Result<String, ErrorReply> {
    let parent = project.join(WORKTREES_DIR);
    fs::create_dir_all(&parent).map_err(|e| io_err(&parent, "create", e))?;
    for n in 1u32.. {
        let name = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        match fs::create_dir(parent.join(&name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(&parent.join(&name), "create", e)),
        }
    }
    unreachable!("the counter does not run out")
}

fn not_empty(dir: &Path) -> ErrorReply {
    RdyError::new(
        E_BAD_INPUT,
        format!("{} is not empty", dir.display()),
        "name a new directory, or an empty one",
    )
    .into()
}

fn add(env: &Env, dir: &Path, view: String) -> Result<(Value, String), ErrorReply> {
    let dir = absolute(dir)?;
    let project = absolute(&env.project)?;
    let created = match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {
            let mut rd = fs::read_dir(&dir).map_err(|e| io_err(&dir, "read", e))?;
            if rd.next().is_some() {
                return Err(not_empty(&dir));
            }
            false
        }
        Ok(_) => return Err(not_empty(&dir)),
        Err(e) if e.kind() == ErrorKind::NotFound => true,
        Err(e) => return Err(io_err(&dir, "read", e)),
    };
    let dir_text = dir
        .to_str()
        .ok_or_else(|| {
            ErrorReply::new(
                E_BAD_INPUT,
                format!("{} is not valid UTF-8", dir.display()),
                "pass a directory whose path is valid UTF-8",
            )
        })?
        .to_string();
    let name = reserve(&project, &sanitized_name(&dir))?;
    let state = project.join(WORKTREES_DIR).join(&name);
    let undo = |dir_made: bool| {
        let _ = fs::remove_dir_all(&state);
        let _ = fs::remove_file(dir.join(POINTER_FILE));
        if dir_made {
            let _ = fs::remove_dir(&dir);
        }
    };
    let setup = (|| -> Result<(), ErrorReply> {
        fs::create_dir_all(&dir).map_err(|e| io_err(&dir, "create", e))?;
        // the name is always quoted: a bare `-` (a sanitized one-character name) is not a YAML string
        let name_q = serde_json::to_string(&name).map_err(|e| io_err(&dir, "encode", e))?;
        let ptr = format!("project: {}\nname: {name_q}\n", pointer_path(&project)?);
        fs::write(dir.join(POINTER_FILE), ptr)
            .map_err(|e| io_err(&dir.join(POINTER_FILE), "write", e))?;
        fs::write(state.join(WORKTREE_DIR_FILE), &dir_text)
            .map_err(|e| io_err(&state.join(WORKTREE_DIR_FILE), "write", e))?;
        Ok(())
    })();
    if let Err(e) = setup {
        undo(created);
        return Err(e);
    }
    let place = Place {
        root: dir.clone(),
        state: state.clone(),
        project: project.clone(),
    };
    match checkout::checkout_place(env, place, Some(view.clone()), false) {
        Ok((v, _)) => {
            let head = v["head"].clone();
            let text = format!("worktree {name} at {dir_text} bound to {view}");
            Ok((
                json!({"name": name, "dir": dir_text, "view": view, "head": head}),
                text,
            ))
        }
        Err(e) => {
            undo(created);
            Err(e)
        }
    }
}

/// The project path as a YAML scalar that reads back exactly.
fn pointer_path(project: &Path) -> Result<String, ErrorReply> {
    let s = project.to_str().ok_or_else(|| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("{} is not valid UTF-8", project.display()),
            "use a project whose path is valid UTF-8",
        )
    })?;
    let plain = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
        && !s.starts_with('-');
    // a JSON string is a valid double-quoted YAML scalar
    Ok(if plain {
        s.to_string()
    } else {
        serde_json::to_string(s).map_err(|e| io_err(project, "encode", e))?
    })
}

fn ls(env: &Env) -> Result<(Value, String), ErrorReply> {
    let list: Vec<WorktreeInfo> = worktrees(&env.project);
    let text = list
        .iter()
        .map(|w| {
            format!(
                "{}  {}  {}",
                w.name,
                w.dir,
                w.view.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let v = serde_json::to_value(&list).unwrap_or(Value::Null);
    Ok((v, text))
}

fn rm(env: &Env, dir: &Path, force: bool) -> Result<(Value, String), ErrorReply> {
    let dir = absolute(dir)?;
    let not_a_worktree = || {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("{} is not a worktree of this project", dir.display()),
            "name a directory that rdy worktree ls shows",
        )
    };
    let place = fs::symlink_metadata(dir.join(POINTER_FILE))
        .ok()
        .filter(|m| m.is_file())
        .and_then(|_| Place::from_pointer(&dir))
        .ok_or_else(not_a_worktree)?;
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if !same(&place.project, &env.project) {
        return Err(not_a_worktree());
    }
    let name = place
        .state
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let Open { place, lock, state } = open_at(env, place, false)?;
    if !force {
        let (_pack, wt) = tree(env, &place)?;
        let dirty = dirty_paths(&place, &state, &wt);
        if !dirty.is_empty() {
            return Err(RdyError::new(
                E_WC_DIRTY,
                format!(
                    "uncommitted edits in {} file(s): {}",
                    dirty.len(),
                    dirty
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "commit them, or pass --force to discard them",
            )
            .into());
        }
    }
    let removed = remove_tracked(&place, &state)?;
    let ptr = place.root.join(POINTER_FILE);
    fs::remove_file(&ptr).map_err(|e| io_err(&ptr, "remove", e))?;
    fs::remove_dir_all(&place.state).map_err(|e| io_err(&place.state, "remove", e))?;
    drop(lock);
    let _ = fs::remove_dir(&place.root); // only when empty
    Ok((
        json!({"name": name, "dir": place.root, "removed": removed}),
        format!("removed worktree {name}"),
    ))
}
