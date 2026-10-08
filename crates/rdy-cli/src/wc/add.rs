//! `rdy add <path>...` (wcad1): stages files, directories, deletions and renames for the next
//! commit. Every path is checked before anything is staged, so one refusal stages nothing.

use super::{Open, open, tree};
use crate::Env;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::layout::{THEIRS_SUFFIX, WcTree};
use rdy_engine::wc::state::{Place, WcState, stage_clear, stage_put};
use rdy_engine::{E_BAD_INPUT, E_NOT_FOUND, E_WC_INVALID, RdyError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// What `rdy add` does with one path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    /// Stage the file's bytes (or clear its stage entry when they equal the base copy).
    Put,
    /// Stage the file's deletion.
    Delete,
}

fn refuse(code: &'static str, message: String, fix: impl Into<String>) -> ErrorReply {
    RdyError::new(code, message, fix).into()
}

/// `p` (as typed, relative to `cwd`) as a path under `root`, without `.`/`..`: `""` is the root.
/// `None` when it leaves the root.
pub(super) fn under_root(root: &Path, cwd: &Path, p: &Path) -> Option<String> {
    fn lexical(p: &Path) -> Option<PathBuf> {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                other => out.push(other),
            }
        }
        Some(out)
    }
    let full = lexical(&cwd.join(p))?;
    let rel = match full.strip_prefix(lexical(root)?) {
        Ok(r) => r.to_path_buf(),
        Err(_) => {
            // the same place spelled through a link (/tmp and /private/tmp)
            let croot = root.canonicalize().ok()?;
            let cfull = full.parent()?.canonicalize().ok()?.join(full.file_name()?);
            cfull.strip_prefix(croot).ok()?.to_path_buf()
        }
    };
    let parts: Option<Vec<&str>> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    Some(parts?.join("/"))
}

/// Whether `path` is `dir` or lies under it (`dir` is `""` for the root).
fn is_under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// The actions one path asks for, or the refusal.
fn plan_one(
    place: &Place,
    st: &WcState,
    wt: &WcTree,
    typed: &str,
    rel: &str,
    acts: &mut BTreeMap<String, Act>,
) -> Result<(), ErrorReply> {
    let last = rel.rsplit('/').next().unwrap_or(rel);
    if !rel.is_empty() && last.ends_with(THEIRS_SUFFIX) {
        return Err(refuse(
            E_BAD_INPUT,
            format!("{typed} holds the store's side of a conflict and is never staged"),
            format!("resolve the conflict, then delete {typed}"),
        ));
    }
    let gone = |p: &str| fs::symlink_metadata(place.root.join(p)).is_err();
    let deletions = |acts: &mut BTreeMap<String, Act>| {
        for p in st.base.files.keys() {
            if is_under(p, rel) && !wt.files.contains_key(p) && gone(p) {
                acts.insert(p.clone(), Act::Delete);
            }
        }
    };
    match fs::symlink_metadata(place.root.join(rel)) {
        Ok(m) if m.file_type().is_dir() => {
            let members: Vec<&String> = wt.files.keys().filter(|p| is_under(p, rel)).collect();
            if members.is_empty()
                && !rel.is_empty()
                && !st.base.files.keys().any(|p| is_under(p, rel))
            {
                return Err(not_in_tree(typed));
            }
            for p in members {
                acts.insert(p.clone(), Act::Put);
            }
            deletions(acts);
            Ok(())
        }
        Ok(_) => {
            if wt.files.contains_key(rel) {
                acts.insert(rel.to_string(), Act::Put);
                Ok(())
            } else if let Some(u) = wt.untracked.iter().find(|u| u.path == rel)
                && rel.ends_with(".yaml")
            {
                Err(refuse(
                    E_WC_INVALID,
                    format!("{typed} is not a doc file: {}", u.reason),
                    format!("rdy check {typed}"),
                ))
            } else {
                Err(not_in_tree(typed))
            }
        }
        Err(_) => {
            if st.base.files.contains_key(rel) {
                acts.insert(rel.to_string(), Act::Delete);
                Ok(())
            } else if st.base.files.keys().any(|p| is_under(p, rel)) {
                deletions(acts);
                Ok(())
            } else {
                Err(refuse(
                    E_NOT_FOUND,
                    format!("{typed} does not exist"),
                    "check the path, relative to the current directory",
                ))
            }
        }
    }
}

fn not_in_tree(typed: &str) -> ErrorReply {
    refuse(
        E_BAD_INPUT,
        format!("{typed} is not part of the working tree"),
        "only docs, _folder.yaml files and their directories are staged",
    )
}

pub(super) fn run(env: &Env, paths: Vec<PathBuf>) -> Result<(Value, String), ErrorReply> {
    let Open {
        place,
        lock,
        mut state,
    } = open(env, true)?;
    let (_pack, wt) = tree(env, &place)?;
    let mut acts: BTreeMap<String, Act> = BTreeMap::new();
    for p in &paths {
        let typed = p.display().to_string();
        let rel = under_root(&place.root, &env.cwd, p).ok_or_else(|| {
            refuse(
                E_BAD_INPUT,
                format!("{typed} is outside the working copy"),
                "give a path inside the working copy",
            )
        })?;
        plan_one(&place, &state, &wt, &typed, &rel, &mut acts)?;
    }
    // the bytes first, so a file that cannot be read stages nothing
    let mut put: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (p, act) in &acts {
        if *act == Act::Put {
            let bytes = fs::read(place.root.join(p)).map_err(|e| {
                refuse(
                    rdy_engine::E_IO,
                    format!("cannot read {p}: {e}"),
                    "check the file, then retry",
                )
            })?;
            put.insert(p.clone(), bytes);
        }
    }
    let mut staged = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    for (p, bytes) in &put {
        state.stage.deleted.remove(p);
        if fs::read(place.base_file(p)).is_ok_and(|b| b == *bytes) {
            stage_clear(&place, p)?;
        } else {
            stage_put(&place, p, bytes)?;
            staged.insert(p.clone());
        }
    }
    for (p, act) in &acts {
        if *act == Act::Delete {
            stage_clear(&place, p)?;
            state.stage.deleted.insert(p.clone());
            deleted.insert(p.clone());
        }
    }
    state.save(&place)?;
    drop(lock);
    let text = format!(
        "staged {} file(s), {} deletion(s)",
        staged.len(),
        deleted.len()
    );
    Ok((json!({"staged": staged, "deleted": deleted}), text))
}
