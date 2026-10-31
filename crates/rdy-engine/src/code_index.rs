//! The code index (cidx1, FW-C1): which commits of a linked code repository cite which blocks,
//! read from their `Implements:` trailers and keyed by the files those commits touched.
//!
//! It is derived from git and never stored: a project names its code repos under `code_repos` in
//! `.rdy.yaml`, each person gives their checkout paths under `code_paths` in `.rdy/local.yaml` or
//! the user config (FW-X5), and the index is rebuilt from `git log` whenever the
//! checkout's HEAD moves (cached per process while it stays). Core only reads git: it runs
//! `git rev-parse`, `git log` and `git ls-tree`, always through [`git_command`] (FW-C2). Counts
//! are reported as they are; nothing here ranks or cuts (FW-C6).
use crate::{E_CONFIG, E_IO, E_LIMIT, RdyError};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// The `.rdy.yaml` key naming the linked code repos; each one's checkout path is under
/// `code_paths` in `.rdy/local.yaml` or the user config (FW-X5).
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

const FIX_TOP: &str = "point code_paths at a git checkout's top folder";
const STDERR_SHOWN: usize = 400;

/// A linked code checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeRepo {
    /// Its name, as `code_repos` lists it.
    pub configured: String,
    /// Its canonical path.
    pub root: PathBuf,
}

/// The project's linked code checkouts, in `code_repos` order, each canonical root once, with the
/// user config of the environment. No key, a null value and an empty list all link none.
pub fn code_repos(project: &Path) -> Result<Vec<CodeRepo>, RdyError> {
    code_repos_with(
        project,
        crate::config_levels::user_file_from_env().as_deref(),
    )
}

/// [`code_repos`] with the user config at `user_file`. The names and paths come from
/// [`linked_repos`](crate::config_levels::linked_repos); its warnings reach the config warnings,
/// not this list. Each path must be a directory (O10).
pub fn code_repos_with(
    project: &Path,
    user_file: Option<&Path>,
) -> Result<Vec<CodeRepo>, RdyError> {
    let (linked, _warnings) = crate::config_levels::linked_repos(project, user_file)?;
    let mut repos: Vec<CodeRepo> = Vec::with_capacity(linked.len());
    for l in linked {
        let missing = || {
            RdyError::new(
                E_CONFIG,
                format!(
                    "code repo {}: no such directory {}",
                    l.name,
                    l.path.display()
                ),
                FIX_TOP,
            )
        };
        let root = std::fs::canonicalize(&l.path).map_err(|_| missing())?;
        if !root.is_dir() {
            return Err(missing());
        }
        if repos.iter().all(|r| r.root != root) {
            repos.push(CodeRepo {
                configured: l.name,
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
/// panics. The only bound is the size of `out`; evidence reads use `parse_log_bounded`.
pub fn parse_log(out: &[u8], nonce: &str) -> Vec<Commit> {
    parse_log_bounded(out, nonce, usize::MAX, usize::MAX).unwrap_or_default()
}

/// `parse_log` for output from a repository the person does not control. Every path token
/// counts (a rename holds two), whatever its encoding: more than `max_paths` in all is
/// `E_LIMIT` `paths`, and one over `max_path` bytes is `E_LIMIT` `path bytes`. Both checks run
/// on the raw token before a path is copied.
pub(crate) fn parse_log_bounded(
    out: &[u8],
    nonce: &str,
    max_paths: usize,
    max_path: usize,
) -> Result<Vec<Commit>, RdyError> {
    let record = [&[0x1e], nonce.as_bytes()].concat();
    let unit = [&[0x1f], nonce.as_bytes()].concat();
    let mut commits = Vec::new();
    let mut bounds = PathBounds {
        left: max_paths,
        max_paths,
        max_path,
    };
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
            changes: changes_of(tail, &mut bounds)?,
        });
    }
    Ok(commits)
}

/// What is left of the path budget of one log.
struct PathBounds {
    left: usize,
    max_paths: usize,
    max_path: usize,
}

impl PathBounds {
    /// Counts one path token and returns it, or the `E_LIMIT` it passes.
    fn take<'a>(&mut self, token: &'a [u8]) -> Result<&'a [u8], RdyError> {
        if self.left == 0 {
            return Err(limit_error(self.max_paths, "paths"));
        }
        self.left -= 1;
        if token.len() > self.max_path {
            return Err(limit_error(self.max_path, "path bytes"));
        }
        Ok(token)
    }
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
pub(crate) fn is_hash(h: &[u8]) -> bool {
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

/// The `--name-status -z` list: a status token, then one path (two for `R` and `C`). Git ends
/// the format with a NUL and puts a newline before the first status token, so only a status
/// token loses its leading newlines, and a token left empty there is no status. A path token is
/// kept byte for byte: a name may start with a newline, or be one. An empty path token is a
/// malformed record and ends the list. Each path token is counted against `bounds` before it is
/// decoded.
fn changes_of(tail: &[u8], bounds: &mut PathBounds) -> Result<Vec<FileChange>, RdyError> {
    let mut raw = tail.split(|b| *b == 0);
    // a path token as written; none when the list ends or the token is empty
    fn path(t: Option<&[u8]>) -> Option<&[u8]> {
        t.filter(|t| !t.is_empty())
    }
    let utf8 = |t: &[u8]| std::str::from_utf8(t).ok().map(str::to_string);
    let mut changes = Vec::new();
    while let Some(t) = raw.next() {
        let lead = t.iter().take_while(|b| **b == b'\n').count();
        // an empty or newline-only token in a status's place separates; it names nothing
        let Some(&kind) = t.get(lead) else {
            continue;
        };
        match kind {
            b'R' | b'C' => {
                let (Some(from), Some(to)) = (path(raw.next()), path(raw.next())) else {
                    break;
                };
                let (from, to) = (bounds.take(from)?, bounds.take(to)?);
                if let (Some(from), Some(to)) = (utf8(from), utf8(to)) {
                    changes.push(if kind == b'R' {
                        FileChange::Rename { from, to }
                    } else {
                        FileChange::Touch(to)
                    });
                }
            }
            _ => {
                let Some(p) = path(raw.next()) else {
                    break;
                };
                let p = bounds.take(p)?;
                if let Some(p) = utf8(p) {
                    changes.push(if kind == b'D' {
                        FileChange::Delete(p)
                    } else {
                        FileChange::Touch(p)
                    });
                }
            }
        }
    }
    Ok(changes)
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

/// The most bytes one git command may print to stdout.
pub const MAX_GIT_OUTPUT: usize = 268_435_456;
/// The most bytes one git command may print to stderr.
pub const MAX_GIT_ERROR: usize = 65_536;
/// The most paths one commit's tree may hold.
pub const MAX_TREE_PATHS: usize = 1_000_000;
/// The longest path, in bytes, a commit's tree may hold.
pub const MAX_TREE_PATH: usize = 4096;

/// How much of a git command's output is kept.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) stdout: usize,
    pub(crate) stderr: usize,
}

impl Limits {
    pub(crate) const GIT: Limits = Limits {
        stdout: MAX_GIT_OUTPUT,
        stderr: MAX_GIT_ERROR,
    };
}

/// The refusal for a read past a bound; `unit` is `output bytes`, `error bytes`, `paths` or
/// `path bytes`.
pub(crate) fn limit_error(n: usize, unit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("code evidence exceeds {n} {unit}"),
        "use a smaller linked checkout",
    )
}

/// What reading one pipe gave.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Drained {
    Done(Vec<u8>),
    /// More than the limit: nothing more is kept.
    Over,
}

/// Reads `r` to its end, keeping at most `limit` bytes. Memory is reserved before it is used, so
/// a refused reservation counts as passing the limit instead of aborting.
pub(crate) fn drain<R: Read>(mut r: R, limit: usize) -> std::io::Result<Drained> {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) => return Ok(Drained::Done(kept)),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n > limit - kept.len() || kept.try_reserve(n).is_err() {
            return Ok(Drained::Over);
        }
        kept.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
thread_local! {
    /// How many git children this thread's bounded runs have waited for.
    pub(crate) static REAPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn io_failed(what: &str, e: &std::io::Error) -> RdyError {
    RdyError::new(E_IO, format!("{what}: {e}"), "check that git runs")
}

/// Runs a git command to completion; a spawn failure is `E_IO`. Its output is bounded
/// ([`Limits::GIT`]).
fn run(c: Command) -> Result<Output, RdyError> {
    run_with(c, Limits::GIT)
}

/// [`run`] with the output kept to `lim`. Both pipes are read while the child runs, each by its
/// own thread. When either passes its bound the child is killed, and the threads and the child
/// are joined before the `E_LIMIT` returns.
pub(crate) fn run_with(mut c: Command, lim: Limits) -> Result<Output, RdyError> {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RdyError::new(
                E_IO,
                "git is not installed or not on PATH",
                "install git: rdy reads linked code repos with git log",
            )
        } else {
            RdyError::new(E_IO, format!("running git: {e}"), "check that git runs")
        }
    })?;
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RdyError::new(
            E_IO,
            "running git: no output pipe",
            "check that git runs",
        ));
    };
    let (tx, rx) = mpsc::channel::<bool>();
    let spawn = |r: Box<dyn Read + Send>, limit: usize| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let got = drain(r, limit);
            let over = !matches!(got, Ok(Drained::Done(_)));
            let _ = tx.send(over);
            got
        })
    };
    let out_thread = spawn(Box::new(out), lim.stdout);
    let err_thread = spawn(Box::new(err), lim.stderr);
    drop(tx);
    for _ in 0..2 {
        match rx.recv() {
            Ok(false) => {}
            Ok(true) => {
                // The child may be blocked writing to a pipe nobody reads any more.
                let _ = child.kill();
                break;
            }
            Err(_) => break,
        }
    }
    let out = out_thread.join();
    let err = err_thread.join();
    let status = child.wait();
    #[cfg(test)]
    REAPED.with(|n| n.set(n.get() + 1));
    let joined = |r: std::thread::Result<std::io::Result<Drained>>| {
        r.unwrap_or_else(|_| Err(std::io::Error::other("reader panicked")))
    };
    match (joined(out), joined(err)) {
        (Ok(Drained::Over), _) => Err(limit_error(lim.stdout, "output bytes")),
        (_, Ok(Drained::Over)) => Err(limit_error(lim.stderr, "error bytes")),
        (Err(e), _) | (_, Err(e)) => Err(io_failed("reading git output", &e)),
        (Ok(Drained::Done(stdout)), Ok(Drained::Done(stderr))) => Ok(Output {
            status: status.map_err(|e| io_failed("waiting for git", &e))?,
            stdout,
            stderr,
        }),
    }
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
pub(crate) fn check_top(repo: &CodeRepo) -> Result<(), RdyError> {
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
            format!("set code_paths.{} to {}", repo.configured, top.display()),
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
pub(crate) fn head_of(repo: &CodeRepo) -> Result<Option<String>, RdyError> {
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

/// The regular, executable and symlink files of the commit `head`, as path → blob id, read
/// from `git ls-tree -rz --full-tree`. Directories and submodule entries are left out, and so is
/// a path that is not UTF-8. Worktree and index changes play no part.
pub(crate) fn read_tree(repo: &CodeRepo, head: &str) -> Result<BTreeMap<String, String>, RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["ls-tree", "-rz", "--full-tree"]).arg(head);
    let tree = run(c)?;
    if !tree.status.success() {
        return Err(git_failed(
            repo,
            "ls-tree",
            &tree,
            "check the repository with git ls-tree",
        ));
    }
    parse_tree(&tree.stdout, MAX_TREE_PATHS, MAX_TREE_PATH)
}

/// The files in `git ls-tree -rz` output (`<mode> <type> <blob>\t<path>` and a NUL each). More
/// than `max_paths` entries or a path over `max_path` bytes is `E_LIMIT`. A malformed entry is
/// skipped.
pub(crate) fn parse_tree(
    out: &[u8],
    max_paths: usize,
    max_path: usize,
) -> Result<BTreeMap<String, String>, RdyError> {
    let mut files = BTreeMap::new();
    for (seen, entry) in out.split(|b| *b == 0).filter(|e| !e.is_empty()).enumerate() {
        if seen >= max_paths {
            return Err(limit_error(max_paths, "paths"));
        }
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let (meta, path) = (&entry[..tab], &entry[tab + 1..]);
        if path.len() > max_path {
            return Err(limit_error(max_path, "path bytes"));
        }
        let mut parts = meta.split(|b| *b == b' ');
        let (Some(mode), Some(kind), Some(blob), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A submodule is a `commit` entry (mode 160000); a tree only shows without `-r`.
        if kind != b"blob" || !matches!(mode, b"100644" | b"100755" | b"120000") || !is_hash(blob) {
            continue;
        }
        if let (Ok(path), Ok(blob)) = (std::str::from_utf8(path), std::str::from_utf8(blob)) {
            files.insert(path.to_string(), blob.to_string());
        }
    }
    Ok(files)
}

/// The commits of `head` as an index limited to the paths in `at_head`; `head` is a commit.
fn build_from(
    repo: &CodeRepo,
    head: String,
    at_head: &BTreeSet<String>,
) -> Result<CodeIndex, RdyError> {
    build_from_bounded(repo, head, at_head, MAX_TREE_PATHS, MAX_TREE_PATH)
}

/// `build_from` with the history path bounds named, so a test can use small ones.
pub(crate) fn build_from_bounded(
    repo: &CodeRepo,
    head: String,
    at_head: &BTreeSet<String>,
    max_paths: usize,
    max_path: usize,
) -> Result<CodeIndex, RdyError> {
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
    Ok(CodeIndex::from_commits(
        &parse_log_bounded(&log.stdout, &nonce, max_paths, max_path)?,
        Some(at_head),
        Some(head),
    ))
}

/// The index of `head` (which HEAD named when it was read), read at that hash rather than at
/// `HEAD`, so HEAD moving meanwhile cannot give an index labelled with another commit.
fn build_at(repo: &CodeRepo, head: String) -> Result<CodeIndex, RdyError> {
    check_not_shallow(repo)?;
    let tree = read_tree(repo, &head)?;
    build_from(repo, head, &tree.keys().cloned().collect())
}

/// The index of the commit `head` whose files are `at_head`, from the cache while it holds that
/// commit's index.
pub(crate) fn index_at(
    repo: &CodeRepo,
    head: &str,
    at_head: &BTreeSet<String>,
) -> Result<Arc<CodeIndex>, RdyError> {
    if let Some(idx) = lock(&CACHE).get(&repo.root)
        && idx.head.as_deref() == Some(head)
    {
        return Ok(Arc::clone(idx));
    }
    let idx = Arc::new(build_from(repo, head.to_string(), at_head)?);
    lock(&CACHE).insert(repo.root.clone(), Arc::clone(&idx));
    Ok(idx)
}

/// Whether `hash` names a commit in the repository. No fetch is tried for a missing one.
pub(crate) fn commit_exists(repo: &CodeRepo, hash: &str) -> Result<bool, RdyError> {
    commit_exists_with(repo, hash, Limits::GIT)
}

/// [`commit_exists`] with git's output kept to `lim`.
pub(crate) fn commit_exists_with(
    repo: &CodeRepo,
    hash: &str,
    lim: Limits,
) -> Result<bool, RdyError> {
    if !is_hash(hash.as_bytes()) {
        return Ok(false);
    }
    let mut c = git_command(&repo.root);
    c.args(["rev-parse", "-q", "--verify"])
        .arg(format!("{hash}^{{commit}}"));
    let out = run_with(c, lim)?;
    if out.status.success() {
        return Ok(true);
    }
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Ok(false);
    }
    Err(git_failed(
        repo,
        "rev-parse",
        &out,
        "check the repository with git fsck",
    ))
}

/// The non-merge commits reachable from `head` and not from `first`: the hashes only. With no
/// `first`, every commit `head` reaches. Both are commits that exist.
pub(crate) fn commits_since(
    repo: &CodeRepo,
    head: &str,
    first: Option<&str>,
) -> Result<HashSet<String>, RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["rev-list", "--no-merges", head]);
    if let Some(first) = first {
        c.arg(format!("^{first}"));
    }
    c.arg("--");
    let out = run(c)?;
    if !out.status.success() {
        return Err(git_failed(
            repo,
            "rev-list",
            &out,
            "check the repository with git log",
        ));
    }
    Ok(out
        .stdout
        .split(|b| *b == b'\n')
        .filter(|h| is_hash(h))
        .filter_map(|h| std::str::from_utf8(h).ok().map(str::to_string))
        .collect())
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
