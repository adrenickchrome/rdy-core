//! The code evidence of two checks (D-54CKQ, D-RVM29): line diffs of a decision's anchored files
//! and excerpts of them, read from commits through bounded git commands. The worktree and the
//! index play no part, no git attribute or driver runs, and a confirmation's text stays text.
use super::case::{Digest, Omitted, clip};
use super::catalogue::AgentCheck;
use crate::code_evidence::{CodeEvidence, anchored_files, at};
use crate::code_index::CODE_REPOS_KEY;
use crate::code_index::{
    CodeRepo, Limits, MAX_GIT_ERROR, check_top, git_command, head_of, run_with,
};
use crate::config_levels::{PROJECT_FILE, is_repo_name, linked_repos, user_file_from_env};
use crate::{E_BAD_INPUT, E_CONFIG, E_IO, Materialized, RdyError};
use rdy_model::BlockId;
use rdy_model::check_request::{CheckCode, MAX_NAME_BYTES, MAX_REVISION_BYTES};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

/// The most files one code case shows.
const MAX_FILES: usize = 64;
/// The most stdout bytes one git read drains.
const MAX_DRAIN: usize = 1_048_576;
/// The most code text bytes one case keeps.
const MAX_CODE_TEXT: usize = 65_536;
/// The most bytes one excerpt keeps.
const MAX_EXCERPT: usize = 16_384;
/// Where git itself looks for a NUL when it asks whether a file is binary.
const BINARY_PROBE: usize = 8_000;

const LIMITS: Limits = Limits {
    stdout: MAX_DRAIN,
    stderr: MAX_GIT_ERROR,
};

fn bad_range() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "code range must be <repo>@<from>..<to>",
        "name a linked repo and two commits",
    )
}

fn no_checkout(check: AgentCheck, repo: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "check {} needs a readable checkout for {repo}",
            check.spec().name
        ),
        format!("point code_paths at a checkout of {repo}, then run or request the check again"),
    )
}

fn no_anchors(check: AgentCheck, decision: &BlockId) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "check {} has no anchored code for {decision}",
            check.spec().name
        ),
        "add code anchors to the decision or its implementation blocks",
    )
}

fn in_bounds(code: &CheckCode) -> bool {
    (1..=MAX_NAME_BYTES).contains(&code.repo.len())
        && (1..=MAX_REVISION_BYTES).contains(&code.from.len())
        && (1..=MAX_REVISION_BYTES).contains(&code.to.len())
}

/// A range as written: `<repo>@<from>..<to>`, with one `@` and one `..`, inside the stored bounds.
/// It reads no repository.
pub fn parse_code_range(text: &str) -> Result<CheckCode, RdyError> {
    let (repo, range) = text.split_once('@').ok_or_else(bad_range)?;
    let (from, to) = range.split_once("..").ok_or_else(bad_range)?;
    if range.contains('@') || to.contains("..") || to.starts_with('.') {
        return Err(bad_range());
    }
    let code = CheckCode {
        repo: repo.to_string(),
        from: from.to_string(),
        to: to.to_string(),
    };
    if !in_bounds(&code) {
        return Err(bad_range());
    }
    Ok(code)
}

/// The range `text` with both revisions resolved to full commit ids in its linked checkout: what
/// a request stores.
pub fn resolve_code_range(
    check: AgentCheck,
    project: &Path,
    text: &str,
) -> Result<CheckCode, RdyError> {
    let code = parse_code_range(text)?;
    let repo = linked_checkout(check, project, &code.repo)?;
    Ok(CheckCode {
        from: resolve_commit(&repo, &code.from)?,
        to: resolve_commit(&repo, &code.to)?,
        repo: code.repo,
    })
}

/// The linked checkout of `name`. A repo with no path here, a path that has gone and a folder
/// that is no usable top-level repository all refuse in one way, and no path enters the text. A
/// failure to run git keeps its own error.
fn linked_checkout(check: AgentCheck, project: &Path, name: &str) -> Result<CodeRepo, RdyError> {
    let (linked, _) = linked_repos(project, user_file_from_env().as_deref())?;
    let entry = linked
        .into_iter()
        .find(|l| l.name == name)
        .ok_or_else(|| no_checkout(check, name))?;
    let root = std::fs::canonicalize(&entry.path).map_err(|_| no_checkout(check, name))?;
    if !root.is_dir() {
        return Err(no_checkout(check, name));
    }
    let repo = CodeRepo {
        configured: entry.name,
        root,
    };
    check_top(&repo).map_err(|e| readable(check, name, e))?;
    Ok(repo)
}

/// `e`, with a checkout's configuration refusal in this module's words.
fn readable(check: AgentCheck, repo: &str, e: RdyError) -> RdyError {
    if e.code == E_CONFIG {
        no_checkout(check, repo)
    } else {
        e
    }
}

/// A failed git read: the existing `E_IO`, in the code index's words.
fn git_failed(repo: &CodeRepo, what: &str, out: &Output) -> RdyError {
    const SHOWN: usize = 400;
    let err = &out.stderr[..out.stderr.len().min(SHOWN)];
    RdyError::new(
        E_IO,
        format!(
            "git {what} failed in code repo {}: {}",
            repo.configured,
            String::from_utf8_lossy(err).trim()
        ),
        "check the repository with git fsck",
    )
}

/// The full commit `revision` names. It reaches git as one argument after `--end-of-options`, so
/// an option-shaped revision is only a name git does not know.
fn resolve_commit(repo: &CodeRepo, revision: &str) -> Result<String, RdyError> {
    let verify = |name: String| {
        let mut c = git_command(&repo.root);
        c.args(["rev-parse", "-q", "--verify", "--end-of-options"])
            .arg(name);
        run_with(c, LIMITS)
    };
    let not_a_commit = || {
        RdyError::new(
            E_BAD_INPUT,
            format!(
                "code revision {revision} is not a commit in {}",
                repo.configured
            ),
            "name a commit that exists in the linked checkout",
        )
    };
    let out = verify(format!("{revision}^{{commit}}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Err(not_a_commit());
    }
    // git words a name that is some other object as a failure: ask for the name alone
    if verify(revision.to_string())?.status.success() {
        return Err(not_a_commit());
    }
    Err(git_failed(repo, "rev-parse", &out))
}

/// The evidence of `repo` at `head`.
fn evidence(
    check: AgentCheck,
    repo: &CodeRepo,
    head: Option<&str>,
) -> Result<CodeEvidence, RdyError> {
    at(repo, head).map_err(|e| readable(check, &repo.configured, e))
}

/// The union of the anchored files of `causes` at `ev`.
fn anchored(m: &Materialized, causes: &[BlockId], ev: &CodeEvidence) -> BTreeSet<String> {
    causes
        .iter()
        .flat_map(|c| anchored_files(m, c, ev))
        .collect()
}

/// One file of a code case, with both of its blob identities.
pub(super) struct CodeFile {
    repo: CodeRepo,
    from: Option<String>,
    to: String,
    path: String,
    before_blob: Option<String>,
    after_blob: Option<String>,
}

/// Where a case's code rows come from: nothing, line diffs or committed excerpts. Each file is
/// resolved, and refusable, before the block rows are built.
pub(super) enum CodeSource {
    None,
    Diff(Vec<CodeFile>),
    Excerpts(Vec<CodeFile>),
}

/// The diffs of `causes`' anchored files between the two ends of `range`.
pub(super) fn diff_source(
    check: AgentCheck,
    project: &Path,
    range: &CheckCode,
    m: &Materialized,
    causes: &[BlockId],
) -> Result<CodeSource, RdyError> {
    if !in_bounds(range) {
        return Err(bad_range());
    }
    let repo = linked_checkout(check, project, &range.repo)?;
    let from = resolve_commit(&repo, &range.from)?;
    let to = resolve_commit(&repo, &range.to)?;
    let was = evidence(check, &repo, Some(&from))?;
    let now = evidence(check, &repo, Some(&to))?;
    let mut paths = anchored(m, causes, &was);
    paths.extend(anchored(m, causes, &now));
    if paths.is_empty() {
        return Err(no_anchors(check, &causes[0]));
    }
    let files = paths
        .into_iter()
        .map(|path| CodeFile {
            repo: repo.clone(),
            from: Some(from.clone()),
            to: to.clone(),
            before_blob: was.blobs.get(&path).cloned(),
            after_blob: now.blobs.get(&path).cloned(),
            path,
        })
        .collect();
    Ok(CodeSource::Diff(files))
}

/// The names `code_repos` lists, as the linked-repo reader keeps them: a listed repo with no path
/// on this machine still counts, so a case cannot bind to the checkouts one machine holds.
fn listed_repos(project: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(project.join(PROJECT_FILE)).unwrap_or_default();
    let map: BTreeMap<String, serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&text).unwrap_or_default();
    let names: BTreeSet<String> = map
        .get(CODE_REPOS_KEY)
        .and_then(serde_yaml_ng::Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(serde_yaml_ng::Value::as_str)
        .filter(|s| is_repo_name(s))
        .map(str::to_string)
        .collect();
    names.into_iter().collect()
}

/// The excerpts of `causes`' anchored files at each linked repo's HEAD, read once per case.
pub(super) fn excerpt_source(
    check: AgentCheck,
    project: &Path,
    m: &Materialized,
    causes: &[BlockId],
) -> Result<CodeSource, RdyError> {
    // the configuration's own errors first, as every linked read gives them
    linked_repos(project, user_file_from_env().as_deref())?;
    let mut files = vec![];
    for name in listed_repos(project) {
        let repo = linked_checkout(check, project, &name)?;
        let head = head_of(&repo).map_err(|e| readable(check, &name, e))?;
        let ev = evidence(check, &repo, head.as_deref())?;
        let Some(head) = head else { continue };
        for path in anchored(m, causes, &ev) {
            files.push(CodeFile {
                repo: repo.clone(),
                from: None,
                to: head.clone(),
                before_blob: None,
                after_blob: ev.blobs.get(&path).cloned(),
                path,
            });
        }
    }
    if files.is_empty() {
        return Err(no_anchors(check, &causes[0]));
    }
    Ok(CodeSource::Excerpts(files))
}

impl CodeFile {
    fn feed(&self, digest: &mut Digest) {
        digest.str(&self.repo.configured);
        digest.opt(self.from.as_deref());
        digest.str(&self.to);
        digest.str(&self.path);
        digest.opt(self.before_blob.as_deref());
        digest.opt(self.after_blob.as_deref());
    }

    fn row(&self, text: &str, code_cut: bool) -> Value {
        json!({
            "repo": self.repo.configured,
            "from": self.from,
            "to": self.to,
            "path": self.path,
            "before_blob": self.before_blob,
            "after_blob": self.after_blob,
            "text": text,
            "code_cut": code_cut,
        })
    }

    /// The line diff of this file: committed blobs only, literal path. Plumbing, so a person's
    /// diff settings do not reach the case's bytes, and `--no-ext-diff`/`--no-textconv` keep any
    /// driver a git config or attribute names from running.
    fn read_diff(&self, left: &mut usize, omitted: &mut Omitted) -> Result<Value, RdyError> {
        if self.before_blob == self.after_blob {
            return Ok(self.row("", false));
        }
        let from = self.from.as_deref().unwrap_or_default();
        let mut c = git_command(&self.repo.root);
        c.args([
            "--literal-pathspecs",
            "diff-tree",
            "-r",
            "-p",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--unified=3",
        ])
        .args([from, self.to.as_str(), "--", self.path.as_str()]);
        let out = checked(c, &self.repo, "diff-tree")?;
        if is_binary_diff(&out.stdout) {
            omitted.code_bytes += out.stdout.len();
            return Ok(self.row("", true));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (kept, cut) = clip(&text, *left);
        *left -= kept.len();
        omitted.code_bytes += cut;
        Ok(self.row(kept, cut > 0))
    }

    /// The start of this file's committed blob. A file too large to read shows no text and
    /// refuses nothing: its blob id still binds the case.
    fn read_excerpt(&self, left: &mut usize, omitted: &mut Omitted) -> Result<Value, RdyError> {
        let Some(blob) = self.after_blob.as_deref() else {
            return Ok(self.row("", false));
        };
        let mut c = git_command(&self.repo.root);
        c.args(["cat-file", "-s", blob]);
        let size = checked(c, &self.repo, "cat-file")?;
        let size: usize = String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse()
            .unwrap_or(usize::MAX);
        if size > MAX_DRAIN || *left == 0 {
            omitted.code_bytes += size;
            return Ok(self.row("", size > 0));
        }
        let mut c = git_command(&self.repo.root);
        c.args(["cat-file", "blob", blob]);
        let out = checked(c, &self.repo, "cat-file")?;
        let probe = &out.stdout[..out.stdout.len().min(BINARY_PROBE)];
        let Some(text) = std::str::from_utf8(&out.stdout)
            .ok()
            .filter(|_| !probe.contains(&0))
        else {
            omitted.code_bytes += out.stdout.len();
            return Ok(self.row("", true));
        };
        let (kept, cut) = clip(text, MAX_EXCERPT.min(*left));
        *left -= kept.len();
        omitted.code_bytes += cut;
        Ok(self.row(kept, cut > 0))
    }
}

/// `c` run under the case's bounds; a failing git is the existing `E_IO`.
fn checked(c: Command, repo: &CodeRepo, what: &str) -> Result<Output, RdyError> {
    let out = run_with(c, LIMITS)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(git_failed(repo, what, &out))
    }
}

/// Whether a one-file diff is git's binary notice. In a text hunk every line has a prefix, and
/// the notice comes before any hunk, so a file's own words cannot forge it.
fn is_binary_diff(out: &[u8]) -> bool {
    out.split(|b| *b == b'\n')
        .take_while(|l| !l.starts_with(b"@@"))
        .any(|l| l.starts_with(b"Binary files "))
}

impl CodeSource {
    /// Every file feeds `digest`, omitted ones included; at most [`MAX_FILES`] are read. `room`
    /// is what the case's text cap leaves after the block rows. A failed read returns no case.
    pub(super) fn read(
        self,
        digest: &mut Digest,
        omitted: &mut Omitted,
        room: usize,
    ) -> Result<Vec<Value>, RdyError> {
        let (files, diff) = match self {
            CodeSource::None => return Ok(vec![]),
            CodeSource::Diff(files) => (files, true),
            CodeSource::Excerpts(files) => (files, false),
        };
        let mut left = MAX_CODE_TEXT.min(room);
        let mut rows = Vec::with_capacity(files.len().min(MAX_FILES));
        for (i, file) in files.iter().enumerate() {
            file.feed(digest);
            if i >= MAX_FILES {
                omitted.code_files += 1;
            } else if diff {
                rows.push(file.read_diff(&mut left, omitted)?);
            } else {
                rows.push(file.read_excerpt(&mut left, omitted)?);
            }
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::E_LIMIT;
    use crate::code_index::REAPED;

    /// Every git child of a code read is waited for, on success, on a refusal and at a cap.
    #[test]
    fn ck4_code_reads_reap_every_child() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=qa", "-c", "user.email=qa"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "--initial-branch=main"]);
        std::fs::write(dir.join("f.txt"), "a\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "--no-verify", "-m", "one"]);
        let from = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("f.txt"), "x".repeat(MAX_DRAIN + 1)).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "--no-verify", "-m", "two"]);
        let to = git(&["rev-parse", "HEAD"]);
        let repo = CodeRepo {
            configured: "app".into(),
            root: std::fs::canonicalize(dir).unwrap(),
        };
        let reaped = || REAPED.with(std::cell::Cell::get);

        let before = reaped();
        assert_eq!(resolve_commit(&repo, &from).unwrap(), from);
        assert_eq!(reaped(), before + 1, "a success");
        let before = reaped();
        assert_eq!(
            resolve_commit(&repo, "--all").unwrap_err().code,
            E_BAD_INPUT
        );
        assert_eq!(reaped(), before + 1, "a refusal");

        let file = CodeFile {
            repo: repo.clone(),
            from: Some(from),
            to,
            path: "f.txt".into(),
            before_blob: Some("a".into()),
            after_blob: Some("b".into()),
        };
        let before = reaped();
        let mut omitted = Omitted::default();
        let err = file.read_diff(&mut 10, &mut omitted).unwrap_err();
        assert_eq!(err.code, E_LIMIT);
        assert_eq!(reaped(), before + 1, "a reached cap");
    }

    #[test]
    fn ck4_a_files_words_cannot_forge_the_binary_notice() {
        let hunk = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+Binary files a and b differ\n";
        assert!(!is_binary_diff(hunk));
        let notice = b"diff --git a/x b/x\nindex 1..2\nBinary files a/x and b/x differ\n";
        assert!(is_binary_diff(notice));
    }
}
