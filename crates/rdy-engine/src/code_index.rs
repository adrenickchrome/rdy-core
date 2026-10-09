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
/// `git log --format` for one run: per commit, a record separator, the hash, a unit separator,
/// the `Implements:` trailer values joined by `,`, a unit separator; the `--name-status` list
/// follows. Each separator is followed by `nonce`, which a commit message cannot know, so a
/// trailer value holding a separator cannot forge a record.
pub fn log_format(nonce: &str) -> String {
    format!(
        "%x1e{nonce}%H%x1f{nonce}%(trailers:key=Implements,valueonly,separator=%x2C,unfold)%x1f{nonce}"
    )
}
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

/// The project's linked code checkouts, in config order, each canonical root once. No key, a
/// null value and an empty list all link none.
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
    if seq.len() > MAX_CODE_REPOS {
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
/// (`GIT_NO_LAZY_FETCH`, honoured by recent gits only; a partial clone is refused before any
/// read, see `check_top`). Callers append the subcommand.
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

/// The commits in `git log -z --name-status --format=<log_format(nonce)>` output. Records
/// split on the separators followed by `nonce`. A hash that is not 40 or 64 lowercase hex
/// characters skips its record, as do malformed records; non-UTF-8 paths are skipped; it never
/// panics.
pub fn parse_log(out: &[u8], nonce: &str) -> Vec<Commit> {
    let record = [&[0x1e], nonce.as_bytes()].concat();
    let unit = [&[0x1f], nonce.as_bytes()].concat();
    let mut commits = Vec::new();
    // the bytes before the first record marker are no record
    for chunk in split_on(out, &record).skip(1) {
        let Some(i) = find(chunk, &unit) else {
            continue;
        };
        let (hash, rest) = (&chunk[..i], &chunk[i + unit.len()..]);
        let Some(j) = find(rest, &unit) else {
            continue;
        };
        let (trailers, tail) = (&rest[..j], &rest[j + unit.len()..]);
        if !is_hash(hash) {
            continue;
        }
        commits.push(Commit {
            hash: String::from_utf8_lossy(hash).into_owned(),
            implements: implements_of(&String::from_utf8_lossy(trailers)),
            changes: changes_of(tail),
        });
    }
    commits
}

/// The first position of `needle` (not empty) in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The parts of `hay` between the occurrences of `sep` (not empty), the first part included.
fn split_on<'a>(hay: &'a [u8], sep: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
    let mut rest = Some(hay);
    std::iter::from_fn(move || {
        let r = rest?;
        match find(r, sep) {
            Some(i) => {
                rest = Some(&r[i + sep.len()..]);
                Some(&r[..i])
            }
            None => {
                rest = None;
                Some(r)
            }
        }
    })
}

/// A full commit hash: 40 (SHA-1) or 64 (SHA-256) lowercase hex characters.
fn is_hash(h: &[u8]) -> bool {
    matches!(h.len(), 40 | 64) && h.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The trailer values: split on `,`, trimmed, empties, values holding a control character and
/// repeats dropped.
fn implements_of(trailers: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in trailers.split(',').map(str::trim) {
        if !v.is_empty() && !v.chars().any(char::is_control) && !out.iter().any(|x| x == v) {
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

/// `repo.root` is a git checkout's top folder and not a partial clone (checked once per process
/// per root, and again after a read of HEAD fails). The shallow check is in `build_at`.
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
    if is_partial_clone(repo)? {
        // An older git ignores GIT_NO_LAZY_FETCH and would fetch into the repository.
        return Err(RdyError::new(
            E_CONFIG,
            format!("code repo {} is a partial clone", repo.configured),
            "link a full clone: rdy reads every commit and never fetches",
        ));
    }
    lock(&TOPS).insert(repo.root.clone());
    Ok(())
}

/// The repository's config names a promisor remote (`remote.<name>.promisor`) or a partial
/// clone filter (`extensions.partialClone`): a partial clone, missing objects it would fetch.
fn is_partial_clone(repo: &CodeRepo) -> Result<bool, RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["rev-parse", "--git-common-dir"]);
    let out = run(c)?;
    if !out.status.success() {
        return Err(git_failed(
            repo,
            "rev-parse",
            &out,
            "check the repository with git status",
        ));
    }
    let printed = String::from_utf8_lossy(&out.stdout);
    let config = repo
        .root
        .join(printed.trim_end_matches(['\n', '\r']))
        .join("config");
    let text = std::fs::read_to_string(&config).map_err(|e| {
        RdyError::new(
            E_IO,
            format!(
                "code repo {}: reading {}: {e}",
                repo.configured,
                config.display()
            ),
            "check file permissions",
        )
    })?;
    Ok(text.lines().any(|line| {
        let line = line.split(['#', ';']).next().unwrap_or_default();
        let (key, value) = line.split_once('=').unwrap_or((line, "true"));
        let value = value.trim().trim_matches('"').to_ascii_lowercase();
        match key.trim().to_ascii_lowercase().as_str() {
            "partialclone" => !value.is_empty(),
            "promisor" => !matches!(value.as_str(), "false" | "no" | "off" | "0" | ""),
            _ => false,
        }
    }))
}

/// The commit HEAD names; `None` while HEAD is unborn. Any other failure is an error, and the
/// root's top-level check runs again on the next read (the repository may have gone).
fn head_of(repo: &CodeRepo) -> Result<Option<String>, RdyError> {
    let head = read_head(repo);
    if head.is_err() {
        lock(&TOPS).remove(&repo.root);
    }
    head
}

fn read_head(repo: &CodeRepo) -> Result<Option<String>, RdyError> {
    let verify = |what: &str| -> Result<Output, RdyError> {
        let mut c = git_command(&repo.root);
        c.args(["rev-parse", "-q", "--verify", what]);
        run(c)
    };
    let printed = |out: &Output| String::from_utf8_lossy(&out.stdout).trim().to_string();
    let fix = "check the repository with git fsck";
    let commit = verify("HEAD^{commit}")?;
    if commit.status.success() {
        return Ok(Some(printed(&commit)));
    }
    // `-q --verify` fails quietly with status 1 when HEAD names nothing; anything louder (no
    // repository any more, a damaged HEAD file) is an error.
    if commit.status.code() != Some(1) || !commit.stderr.is_empty() {
        return Err(git_failed(repo, "rev-parse", &commit, fix));
    }
    let named = verify("HEAD")?;
    if named.status.success() {
        // HEAD's branch names an object that is missing or not a commit.
        return Err(RdyError::new(
            E_IO,
            format!(
                "git rev-parse failed in code repo {}: HEAD names {}, which is not a commit in the repository",
                repo.configured,
                printed(&named)
            ),
            fix,
        ));
    }
    if named.status.code() != Some(1) || !named.stderr.is_empty() {
        return Err(git_failed(repo, "rev-parse", &named, fix));
    }
    // HEAD's branch has no commit yet. (A branch file holding no hash at all reads the same:
    // git's own `rev-parse` cannot tell the two apart.)
    Ok(None)
}

/// `repo.root` is not a shallow clone. Checked on every build, not once per root: a full
/// checkout can become shallow later (`git fetch --depth=1`), and an index from shallow
/// history attributes every file to its oldest commit.
fn check_not_shallow(repo: &CodeRepo) -> Result<(), RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["rev-parse", "--is-shallow-repository"]);
    let out = run(c)?;
    if !out.status.success() {
        return Err(git_failed(
            repo,
            "rev-parse",
            &out,
            "check the repository with git status",
        ));
    }
    if String::from_utf8_lossy(&out.stdout).trim() == "true" {
        return Err(RdyError::new(
            E_CONFIG,
            format!("code repo {} is a shallow clone", repo.configured),
            "link a full clone (git fetch --unshallow): a shallow clone attributes every file to its oldest commit",
        ));
    }
    Ok(())
}

/// The index of `head` (which HEAD named when it was read), read at that hash rather than at
/// `HEAD`, so HEAD moving meanwhile cannot give an index labelled with another commit.
fn build_at(repo: &CodeRepo, head: String) -> Result<CodeIndex, RdyError> {
    check_not_shallow(repo)?;
    let nonce = crate::wc::state::new_nonce();
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
    .arg(format!("--format={}", log_format(&nonce)))
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
        &parse_log(&log.stdout, &nonce),
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
