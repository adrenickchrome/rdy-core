//! The code index (cidx1, FW-C1): which commits of a linked code repository cite which blocks,
//! read from their `Implements:` trailers and keyed by the files those commits touched.
//!
//! It is derived from git and never stored: a project lists its code checkouts under
//! `code_repos` in `.rdy/config.yaml`, and the index is rebuilt from `git log` whenever the
//! checkout's HEAD moves (cached per process while it stays). Core only reads git: it runs
//! `git rev-parse`, `git log` and `git ls-tree`, always through [`git_command`] (FW-C2). Counts
//! are reported as they are; nothing here ranks or cuts (FW-C6).
use crate::{E_CONFIG, E_IO, RdyError};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// The `.rdy/config.yaml` key listing the linked code checkouts.
pub const CODE_REPOS_KEY: &str = "code_repos";
/// At most this many linked code checkouts.
pub const MAX_CODE_REPOS: usize = 16;
/// `git log --format`: per commit, a record separator, the hash, a unit separator, the
/// `Implements:` trailer values joined by `,`, a unit separator; the `--name-status` list follows.
pub const LOG_FORMAT: &str =
    "%x1e%H%x1f%(trailers:key=Implements,valueonly,separator=%x2C,unfold)%x1f";
/// The caller's git environment that would point git at another repository or config.
pub const GIT_ENV_REMOVED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

const FIX_TOP: &str = "point code_repos at a git checkout's top folder";
const STDERR_SHOWN: usize = 400;

/// A linked code checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeRepo {
    /// The entry as written in `code_repos`.
    pub configured: String,
    /// Its canonical path.
    pub root: PathBuf,
}

/// The project's linked code checkouts, in config order, each canonical root once.
pub fn code_repos(project: &Path) -> Result<Vec<CodeRepo>, RdyError> {
    let text = std::fs::read_to_string(project.join(".rdy/config.yaml"))
        .map_err(|e| RdyError::new(E_IO, e.to_string(), "check file permissions"))?;
    let cfg: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)
        .map_err(|e| RdyError::new(E_CONFIG, e.to_string(), "fix .rdy/config.yaml"))?;
    let entries = match cfg.get(CODE_REPOS_KEY) {
        None | Some(serde_yaml_ng::Value::Null) => return Ok(vec![]),
        Some(v) => v,
    };
    let bad = || {
        RdyError::new(
            E_CONFIG,
            format!(
                "{CODE_REPOS_KEY} must be a list of paths to git checkouts (at most {MAX_CODE_REPOS})"
            ),
            "fix code_repos in .rdy/config.yaml, e.g. code_repos: [../app]",
        )
    };
    let seq = entries.as_sequence().ok_or_else(bad)?;
    if seq.is_empty() || seq.len() > MAX_CODE_REPOS {
        return Err(bad());
    }
    let mut repos: Vec<CodeRepo> = Vec::with_capacity(seq.len());
    for entry in seq {
        let configured = match entry.as_str() {
            Some(s) if !s.is_empty() => s,
            _ => return Err(bad()),
        };
        let missing = || {
            RdyError::new(
                E_CONFIG,
                format!("code repo {configured}: no such directory"),
                FIX_TOP,
            )
        };
        let root = std::fs::canonicalize(project.join(configured)).map_err(|_| missing())?;
        if !root.is_dir() {
            return Err(missing());
        }
        if repos.iter().all(|r| r.root != root) {
            repos.push(CodeRepo {
                configured: configured.to_string(),
                root,
            });
        }
    }
    Ok(repos)
}

/// `git` reading the repository at `root` only: no fsmonitor, quoting, signatures or colour, none
/// of the caller's git environment, and no lazy fetch of a partial clone's missing objects
/// (`GIT_NO_LAZY_FETCH`, git >= 2.45). Callers append the subcommand.
pub fn git_command(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
        "-c",
        "log.showSignature=false",
        "-c",
        "color.ui=false",
    ]);
    for k in GIT_ENV_REMOVED {
        c.env_remove(k);
    }
    c.env("GIT_NO_LAZY_FETCH", "1");
    c.stdin(Stdio::null());
    c
}

/// What one commit did to one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Touch(String),
    Delete(String),
    Rename { from: String, to: String },
}

/// One non-merge commit: its hash, the blocks its trailers cite and the paths it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub implements: Vec<String>,
    pub changes: Vec<FileChange>,
}

/// The commits in `git log -z --name-status --format=<LOG_FORMAT>` output. Malformed records,
/// non-UTF-8 hashes and non-UTF-8 paths are skipped; it never panics.
pub fn parse_log(out: &[u8]) -> Vec<Commit> {
    let mut commits = Vec::new();
    for chunk in out.split(|b| *b == 0x1e) {
        if chunk.is_empty() {
            continue;
        }
        let Some(i) = chunk.iter().position(|b| *b == 0x1f) else {
            continue;
        };
        let (hash, rest) = (&chunk[..i], &chunk[i + 1..]);
        let Some(j) = rest.iter().position(|b| *b == 0x1f) else {
            continue;
        };
        let (trailers, tail) = (&rest[..j], &rest[j + 1..]);
        let Ok(hash) = std::str::from_utf8(hash) else {
            continue;
        };
        commits.push(Commit {
            hash: hash.to_string(),
            implements: implements_of(&String::from_utf8_lossy(trailers)),
            changes: changes_of(tail),
        });
    }
    commits
}

/// The trailer values: split on `,`, trimmed, empties and repeats dropped.
fn implements_of(trailers: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in trailers.split(',').map(str::trim) {
        if !v.is_empty() && !out.iter().any(|x| x == v) {
            out.push(v.to_string());
        }
    }
    out
}

/// The `--name-status -z` list: a status token, then one path (two for `R` and `C`).
fn changes_of(tail: &[u8]) -> Vec<FileChange> {
    let mut tokens = tail
        .split(|b| *b == 0)
        .map(|t| {
            let lead = t.iter().take_while(|b| **b == b'\n').count();
            &t[lead..]
        })
        .filter(|t| !t.is_empty());
    let utf8 = |t: &[u8]| std::str::from_utf8(t).ok().map(str::to_string);
    let mut changes = Vec::new();
    while let Some(status) = tokens.next() {
        match status[0] {
            kind @ (b'R' | b'C') => {
                let (Some(from), Some(to)) = (tokens.next(), tokens.next()) else {
                    break;
                };
                if let (Some(from), Some(to)) = (utf8(from), utf8(to)) {
                    changes.push(if kind == b'R' {
                        FileChange::Rename { from, to }
                    } else {
                        FileChange::Touch(to)
                    });
                }
            }
            kind => {
                let Some(path) = tokens.next() else {
                    break;
                };
                if let Some(path) = utf8(path) {
                    changes.push(if kind == b'D' {
                        FileChange::Delete(path)
                    } else {
                        FileChange::Touch(path)
                    });
                }
            }
        }
    }
    changes
}

/// One linked repository's citations at one HEAD.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeIndex {
    /// The commit indexed; `None` for a repository with no commits.
    pub head: Option<String>,
    /// How many non-merge commits were read.
    pub commits: usize,
    /// Path at HEAD → cited text → the commits citing it that touched the path, in order.
    pub files: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// Cited text → every commit citing it, oldest first, whatever files it touched.
    pub cited: BTreeMap<String, Vec<String>>,
}

impl CodeIndex {
    /// Replays `commits` (oldest first): renames carry a path's citations, deletes drop them.
    /// With `at_head`, only those paths are kept.
    pub fn from_commits(
        commits: &[Commit],
        at_head: Option<&BTreeSet<String>>,
        head: Option<String>,
    ) -> CodeIndex {
        let mut idx = CodeIndex {
            head,
            commits: commits.len(),
            ..CodeIndex::default()
        };
        for c in commits {
            for text in &c.implements {
                let by = idx.cited.entry(text.clone()).or_default();
                if by.last() != Some(&c.hash) {
                    by.push(c.hash.clone());
                }
            }
            for change in &c.changes {
                let touched = match change {
                    FileChange::Rename { from, to } => {
                        if let Some(src) = idx.files.remove(from) {
                            let dst = idx.files.entry(to.clone()).or_default();
                            for (text, hashes) in src {
                                let into = dst.entry(text).or_default();
                                for h in hashes {
                                    if !into.contains(&h) {
                                        into.push(h);
                                    }
                                }
                            }
                        }
                        to
                    }
                    FileChange::Delete(p) => {
                        idx.files.remove(p);
                        continue;
                    }
                    FileChange::Touch(p) => p,
                };
                if c.implements.is_empty() {
                    continue;
                }
                let file = idx.files.entry(touched.clone()).or_default();
                for text in &c.implements {
                    let by = file.entry(text.clone()).or_default();
                    if !by.contains(&c.hash) {
                        by.push(c.hash.clone());
                    }
                }
            }
        }
        if let Some(at_head) = at_head {
            idx.files.retain(|p, _| at_head.contains(p));
        }
        idx
    }
}

/// Roots whose top-level check passed in this process.
static TOPS: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);
/// The last index built per root.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<CodeIndex>>>> = LazyLock::new(Mutex::default);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs a git command to completion; a spawn failure is `E_IO`.
fn run(mut c: Command) -> Result<Output, RdyError> {
    c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RdyError::new(
                E_IO,
                "git is not installed or not on PATH",
                "install git: rdy reads linked code repos with git log",
            )
        } else {
            RdyError::new(E_IO, format!("running git: {e}"), "check that git runs")
        }
    })
}

/// A failed read of a repository: `E_IO` carrying the start of git's stderr.
fn git_failed(repo: &CodeRepo, what: &str, out: &Output, fix: &str) -> RdyError {
    let err = &out.stderr[..out.stderr.len().min(STDERR_SHOWN)];
    RdyError::new(
        E_IO,
        format!(
            "git {what} failed in code repo {}: {}",
            repo.configured,
            String::from_utf8_lossy(err).trim()
        ),
        fix,
    )
}

/// `repo.root` is a git checkout's top folder (checked once per process per root).
fn check_top(repo: &CodeRepo) -> Result<(), RdyError> {
    if lock(&TOPS).contains(&repo.root) {
        return Ok(());
    }
    let mut c = git_command(&repo.root);
    c.args(["rev-parse", "--show-toplevel"]);
    let out = run(c)?;
    if !out.status.success() {
        return Err(RdyError::new(
            E_CONFIG,
            format!("code repo {} is not a git repository", repo.configured),
            FIX_TOP,
        ));
    }
    let printed = String::from_utf8_lossy(&out.stdout);
    let printed = printed.trim_end_matches(['\n', '\r']);
    let top = std::fs::canonicalize(printed).unwrap_or_else(|_| PathBuf::from(printed));
    if top != repo.root {
        return Err(RdyError::new(
            E_CONFIG,
            format!(
                "code repo {} is inside the repository at {}; link its top folder",
                repo.configured,
                top.display()
            ),
            format!("set code_repos to {}", top.display()),
        ));
    }
    lock(&TOPS).insert(repo.root.clone());
    Ok(())
}

/// The commit HEAD names; `None` while HEAD is unborn.
fn head_of(repo: &CodeRepo) -> Result<Option<String>, RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["rev-parse", "-q", "--verify", "HEAD^{commit}"]);
    let out = run(c)?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// The index of `head` (which HEAD named when it was read), read at that hash rather than at
/// `HEAD`, so HEAD moving meanwhile cannot give an index labelled with another commit.
fn build_at(repo: &CodeRepo, head: String) -> Result<CodeIndex, RdyError> {
    let mut c = git_command(&repo.root);
    c.args([
        "log",
        "--no-merges",
        "--reverse",
        "-M",
        "--name-status",
        "-z",
        "--no-color",
    ])
    .arg(format!("--format={LOG_FORMAT}"))
    .args([head.as_str(), "--"]);
    let log = run(c)?;
    if !log.status.success() {
        return Err(git_failed(
            repo,
            "log",
            &log,
            "check the repository with git log; link a full clone: rdy does not fetch",
        ));
    }
    let mut c = git_command(&repo.root);
    c.args(["ls-tree", "-r", "-z", "--name-only", "--full-tree"])
        .arg(&head);
    let tree = run(c)?;
    if !tree.status.success() {
        return Err(git_failed(
            repo,
            "ls-tree",
            &tree,
            "check the repository with git ls-tree",
        ));
    }
    let at_head: BTreeSet<String> = tree
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .filter_map(|p| std::str::from_utf8(p).ok().map(str::to_string))
        .collect();
    Ok(CodeIndex::from_commits(
        &parse_log(&log.stdout),
        Some(&at_head),
        Some(head),
    ))
}

/// The index of the checkout's current HEAD, built afresh (no cache).
pub fn build(repo: &CodeRepo) -> Result<CodeIndex, RdyError> {
    check_top(repo)?;
    match head_of(repo)? {
        None => Ok(CodeIndex::default()),
        Some(head) => build_at(repo, head),
    }
}

/// The index of the checkout's current HEAD, reused while HEAD stays where it was.
pub fn index_of(repo: &CodeRepo) -> Result<Arc<CodeIndex>, RdyError> {
    check_top(repo)?;
    let Some(head) = head_of(repo)? else {
        return Ok(Arc::new(CodeIndex::default()));
    };
    if let Some(idx) = lock(&CACHE).get(&repo.root)
        && idx.head.as_deref() == Some(head.as_str())
    {
        return Ok(Arc::clone(idx));
    }
    let idx = Arc::new(build_at(repo, head)?);
    lock(&CACHE).insert(repo.root.clone(), Arc::clone(&idx));
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=qa",
                "-c",
                "user.email=qa",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(dir: &Path, path: &str, message: &str) -> String {
        std::fs::write(dir.join(path), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--no-verify", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn cidx1_the_index_is_built_at_the_hash_it_is_labelled_with() {
        // HEAD moving between reading the hash and running git log / ls-tree must not
        // give an index labelled with one commit and built from another.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "--initial-branch=main"]);
        let c1 = commit(dir, "a.rs", "feat: a\n\nImplements: c-AAAAA");
        commit(dir, "b.rs", "feat: b\n\nImplements: c-BBBBB");
        let repo = CodeRepo {
            configured: dir.to_string_lossy().into_owned(),
            root: std::fs::canonicalize(dir).unwrap(),
        };
        let idx = build_at(&repo, c1.clone()).unwrap();
        assert_eq!(idx.head.as_deref(), Some(c1.as_str()));
        assert_eq!(idx.commits, 1, "git log ran at HEAD, not at {c1}");
        assert_eq!(
            idx.files.keys().collect::<Vec<_>>(),
            vec!["a.rs"],
            "ls-tree ran at HEAD, not at {c1}"
        );
        assert!(!idx.cited.contains_key("c-BBBBB"));
    }
}
