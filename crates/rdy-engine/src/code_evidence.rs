//! Code evidence at a captured HEAD (cp2): the committed files of a linked code checkout with
//! their blob ids, the citations the code index reads from the commits, and Git reachability.
//!
//! One [`CodeEvidence`] is read once, at one commit: every Git command for it names that commit's
//! hash, so a later commit, a dirty worktree or a staged change cannot move it. It reuses the
//! code index (`code_index`) and the code briefing's citation names (`code_brief`); it adds no
//! second parser, cache or watcher. "Later" is Git reachability, never commit time (D-ERKCP).
use crate::code_brief::{cites, covering_names};
use crate::code_index::{
    CodeIndex, CodeRepo, check_top, code_repos_with, commit_exists, commits_since, head_of,
    index_at, read_tree,
};
use crate::{Materialized, RdyError};
use rdy_model::BlockId;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// The commits of a first HEAD's "later" set, by first HEAD; `None` is a first HEAD this
/// checkout does not hold.
type LaterSets = HashMap<Option<String>, Option<Arc<HashSet<String>>>>;

/// One linked checkout as it stood at one commit.
#[derive(Clone, Debug)]
pub struct CodeEvidence {
    /// The repository's name, as `code_repos` lists it.
    pub repo: String,
    /// Its canonical path.
    pub root: PathBuf,
    /// The commit captured; `None` while HEAD is unborn.
    pub head: Option<String>,
    /// The citations of that commit's history (shared with the code index's cache).
    pub index: Arc<CodeIndex>,
    /// Path → blob id of every regular file, executable file and symlink the commit holds, in
    /// path order. Directories, submodules, the worktree and the index play no part.
    pub blobs: BTreeMap<String, String>,
    /// False when the commit is recorded but this checkout does not hold it: the evidence is
    /// empty, and no later commit can be shown.
    pub available: bool,
    later: Arc<Mutex<LaterSets>>,
}

impl PartialEq for CodeEvidence {
    fn eq(&self, other: &Self) -> bool {
        (
            &self.repo,
            &self.head,
            &self.index,
            &self.blobs,
            self.available,
        ) == (
            &other.repo,
            &other.head,
            &other.index,
            &other.blobs,
            other.available,
        )
    }
}

impl CodeEvidence {
    fn empty(repo: &CodeRepo, head: Option<&str>, available: bool) -> CodeEvidence {
        CodeEvidence {
            repo: repo.configured.clone(),
            root: repo.root.clone(),
            head: head.map(str::to_string),
            index: Arc::new(CodeIndex::default()),
            blobs: BTreeMap::new(),
            available,
            later: Arc::default(),
        }
    }

    fn repo(&self) -> CodeRepo {
        CodeRepo {
            configured: self.repo.clone(),
            root: self.root.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LaterSets> {
        self.later.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The non-merge commits reachable from the captured HEAD and not from `first`; `None` when
    /// `first` is not a commit this checkout holds.
    fn later_than(
        &self,
        head: &str,
        first: Option<&str>,
    ) -> Result<Option<Arc<HashSet<String>>>, RdyError> {
        let key = first.map(str::to_string);
        if let Some(known) = self.lock().get(&key) {
            return Ok(known.clone());
        }
        let repo = self.repo();
        let set = match first {
            Some(f) if !commit_exists(&repo, f)? => None,
            _ => Some(Arc::new(commits_since(&repo, head, first)?)),
        };
        self.lock().insert(key, set.clone());
        Ok(set)
    }
}

/// The evidence of every linked checkout of the project at `root`, sorted by configured repo
/// name. Each HEAD is read once. No linked checkout gives none; a configured checkout that is
/// not a usable repository keeps the code index's `E_CONFIG`.
pub fn capture(root: &Path) -> Result<Vec<CodeEvidence>, RdyError> {
    capture_with(root, crate::config_levels::user_file_from_env().as_deref())
}

/// [`capture`] with the user config at `user_file`.
fn capture_with(root: &Path, user_file: Option<&Path>) -> Result<Vec<CodeEvidence>, RdyError> {
    let mut repos = code_repos_with(root, user_file)?;
    repos.sort_by(|a, b| a.configured.cmp(&b.configured));
    repos
        .iter()
        .map(|repo| {
            check_top(repo)?;
            let head = head_of(repo)?;
            at(repo, head.as_deref())
        })
        .collect()
}

/// The evidence of `repo` at the commit `head`. `None` is an unborn HEAD: no files and no
/// commits. A commit the checkout does not hold gives empty evidence with `available` false; no
/// fetch is tried.
#[allow(dead_code)] // first used by the change-plan tasks
pub(crate) fn at(repo: &CodeRepo, head: Option<&str>) -> Result<CodeEvidence, RdyError> {
    check_top(repo)?;
    let Some(head) = head else {
        return Ok(CodeEvidence::empty(repo, None, true));
    };
    if !commit_exists(repo, head)? {
        return Ok(CodeEvidence::empty(repo, Some(head), false));
    }
    let blobs = read_tree(repo, head)?;
    let at_head: BTreeSet<String> = blobs.keys().cloned().collect();
    let index = index_at(repo, head, &at_head)?;
    Ok(CodeEvidence {
        repo: repo.configured.clone(),
        root: repo.root.clone(),
        head: Some(head.to_string()),
        index,
        blobs,
        available: true,
        later: Arc::default(),
    })
}

/// The files at the captured HEAD whose commits cite `cause`: a cited id, key or section path
/// that names the block, or a section above it (a citation of a section covers every block under
/// it, as the code briefing reads it). One path once, in order.
#[allow(dead_code)] // first used by the change-plan tasks
pub(crate) fn anchored_files(
    m: &Materialized,
    cause: &BlockId,
    evidence: &CodeEvidence,
) -> BTreeSet<String> {
    let covering = covering_names(m, cause);
    let mut known: HashMap<&str, bool> = HashMap::new();
    evidence
        .index
        .files
        .iter()
        .filter(|(path, _)| evidence.blobs.contains_key(*path))
        .filter(|(_, cited)| {
            cited
                .keys()
                .any(|t| *known.entry(t).or_insert_with(|| cites(&covering, t)))
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// Whether a commit made after `first_head` closes `path`: one reachable from the captured HEAD
/// and not from `first_head` (with no `first_head`, any commit the HEAD reaches), that touched
/// the file (following the index's rename rule) and has a trailer reaching one of
/// `allowed_blocks`, by the names `anchored_files` uses. A `first_head` this checkout does not
/// hold, an unborn HEAD and unavailable evidence all give false; nothing is fetched.
#[allow(dead_code)] // first used by the change-plan tasks
pub(crate) fn later_touch(
    evidence: &CodeEvidence,
    first_head: Option<&str>,
    path: &str,
    allowed_blocks: &BTreeSet<BlockId>,
    m: &Materialized,
) -> Result<bool, RdyError> {
    let (true, Some(head)) = (evidence.available, evidence.head.as_deref()) else {
        return Ok(false);
    };
    let Some(cited) = evidence.index.files.get(path) else {
        return Ok(false);
    };
    let covering: Vec<_> = allowed_blocks
        .iter()
        .map(|b| covering_names(m, b))
        .collect();
    let touching: BTreeSet<&str> = cited
        .iter()
        .filter(|(text, _)| covering.iter().any(|c| cites(c, text)))
        .flat_map(|(_, hashes)| hashes.iter().map(String::as_str))
        .collect();
    if touching.is_empty() {
        return Ok(false);
    }
    Ok(evidence
        .later_than(head, first_head)?
        .is_some_and(|later| touching.iter().any(|h| later.contains(*h))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::{
        Drained, Limits, MAX_GIT_ERROR, MAX_GIT_OUTPUT, MAX_TREE_PATH, MAX_TREE_PATHS, REAPED,
        drain, parse_tree, run_with,
    };
    use crate::{E_CONFIG, E_LIMIT};
    use rdy_model::{Block, FieldValue, Op};
    use std::collections::BTreeMap;
    use std::process::Command;

    fn git_env(dir: &Path, date: Option<&str>, args: &[&str]) -> String {
        let mut c = Command::new("git");
        c.arg("-C")
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
            .env("GIT_CONFIG_NOSYSTEM", "1");
        if let Some(d) = date {
            c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        let out = c.output().expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        git_env(dir, None, args)
    }

    fn init(dir: &Path) {
        git(dir, &["init", "-q", "--initial-branch=main"]);
    }

    /// Writes `path` (its content names the message, so every commit changes it), commits, and
    /// returns the commit.
    fn commit_at(dir: &Path, date: Option<&str>, path: &str, message: &str) -> String {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(dir.join(parent)).unwrap();
        }
        std::fs::write(dir.join(path), format!("{message}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git_env(dir, date, &["commit", "-q", "--no-verify", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn commit(dir: &Path, path: &str, message: &str) -> String {
        commit_at(dir, None, path, message)
    }

    fn repo_of(dir: &Path) -> CodeRepo {
        CodeRepo {
            configured: "app".to_string(),
            root: std::fs::canonicalize(dir).unwrap(),
        }
    }

    fn evidence(dir: &Path) -> CodeEvidence {
        let repo = repo_of(dir);
        let head = crate::code_index::head_of(&repo).unwrap();
        at(&repo, head.as_deref()).unwrap()
    }

    fn id(text: &str) -> BlockId {
        BlockId::parse(text).unwrap()
    }

    fn block(text: &str, block_type: &str, parent: Option<&str>, fields: &[(&str, &str)]) -> Block {
        Block {
            id: id(text),
            block_type: block_type.to_string(),
            parent: parent.map(id),
            order: "a0".to_string(),
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
                .collect::<BTreeMap<_, _>>(),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        }
    }

    fn state(blocks: Vec<Block>) -> Materialized {
        let ops: Vec<Op> = blocks
            .into_iter()
            .map(|block| Op::Create { block })
            .collect();
        Materialized::default().with(&ops)
    }

    const X: &str = "c-AAAAA";
    const Y: &str = "c-BBBBB";

    fn flat() -> Materialized {
        state(vec![
            block(X, "decision", None, &[]),
            block(Y, "decision", None, &[]),
        ])
    }

    fn allowed(ids: &[&str]) -> BTreeSet<BlockId> {
        ids.iter().map(|i| id(i)).collect()
    }

    fn files(set: BTreeSet<String>) -> Vec<String> {
        set.into_iter().collect()
    }

    #[test]
    fn cp2_later_means_reachable_not_newer_clock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let first = commit_at(
            dir,
            Some("2030-01-01T00:00:00Z"),
            "f.rs",
            "a\n\nImplements: c-AAAAA",
        );
        // the later commit carries the older clock
        let later = commit_at(
            dir,
            Some("2001-01-01T00:00:00Z"),
            "f.rs",
            "b\n\nImplements: c-AAAAA",
        );
        // a commit with the newest clock that HEAD does not reach
        git(dir, &["checkout", "-q", "-b", "side", &first]);
        commit_at(
            dir,
            Some("2099-01-01T00:00:00Z"),
            "f.rs",
            "s\n\nImplements: c-AAAAA",
        );
        git(dir, &["checkout", "-q", "main"]);
        let (m, ev) = (flat(), evidence(dir));
        assert_eq!(ev.head.as_deref(), Some(later.as_str()));
        assert!(later_touch(&ev, Some(&first), "f.rs", &allowed(&[X]), &m).unwrap());
        assert!(
            !later_touch(&ev, Some(&later), "f.rs", &allowed(&[X]), &m).unwrap(),
            "nothing is later than HEAD itself, whatever the clocks say"
        );
        // from a first HEAD that HEAD does not reach, only the commits it does not share count
        let side = git(dir, &["rev-parse", "side"]);
        assert!(later_touch(&ev, Some(&side), "f.rs", &allowed(&[X]), &m).unwrap());
        // no first HEAD: every commit counts
        assert!(later_touch(&ev, None, "f.rs", &allowed(&[X]), &m).unwrap());
    }

    #[test]
    fn cp2_touch_and_matching_trailer_are_both_required() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let first = commit(dir, "f.rs", "one\n\nImplements: c-AAAAA");
        commit(
            dir,
            "g.rs",
            "cites X, touches another file\n\nImplements: c-AAAAA",
        );
        commit(dir, "f.rs", "touches with no trailer");
        let (m, ev) = (flat(), evidence(dir));
        assert!(
            !later_touch(&ev, Some(&first), "f.rs", &allowed(&[X]), &m).unwrap(),
            "a cited commit that missed the file and an uncited touch close nothing"
        );
        assert!(later_touch(&ev, Some(&first), "g.rs", &allowed(&[X]), &m).unwrap());
        commit(
            dir,
            "f.rs",
            "touches citing another decision\n\nImplements: c-BBBBB",
        );
        let ev = evidence(dir);
        assert!(!later_touch(&ev, Some(&first), "f.rs", &allowed(&[X]), &m).unwrap());
        assert!(later_touch(&ev, Some(&first), "f.rs", &allowed(&[X, Y]), &m).unwrap());
        assert!(!later_touch(&ev, Some(&first), "f.rs", &allowed(&[]), &m).unwrap());
    }

    #[test]
    fn cp2_id_key_and_leaf_section_share_the_resolver() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        commit(dir, "by_id.rs", "i\n\nImplements: c-AAAAA");
        commit(dir, "by_key.rs", "k\n\nImplements: Cause-Key");
        commit(dir, "by_path.rs", "p\n\nImplements: Concepts/Foo");
        commit(dir, "by_doc_id.rs", "d\n\nImplements: d-AAAAA");
        commit(dir, "other.rs", "o\n\nImplements: c-BBBBB");
        commit(dir, "plain.rs", "no trailer");
        let m = state(vec![
            block("fd-AAAAA", "folder", None, &[("name", "concepts")]),
            block("d-AAAAA", "doc", Some("fd-AAAAA"), &[("name", "foo")]),
            block(X, "decision", Some("d-AAAAA"), &[("key", "cause-key")]),
            block(Y, "decision", None, &[]),
        ]);
        let ev = evidence(dir);
        assert_eq!(
            files(anchored_files(&m, &id(X), &ev)),
            ["by_doc_id.rs", "by_id.rs", "by_key.rs", "by_path.rs"],
            "a block is anchored by its id, its key and the section it lies under"
        );
        assert_eq!(
            files(anchored_files(&m, &id("d-AAAAA"), &ev)),
            ["by_doc_id.rs", "by_path.rs"],
            "a citation of a block under the section does not anchor the section"
        );
        assert_eq!(files(anchored_files(&m, &id(Y), &ev)), ["other.rs"]);
        assert!(anchored_files(&m, &id("c-ZZZZZ"), &ev).is_empty());
        // a later commit closes through the same names
        assert!(later_touch(&ev, None, "by_path.rs", &allowed(&[X]), &m).unwrap());
        assert!(!later_touch(&ev, None, "other.rs", &allowed(&[X]), &m).unwrap());
    }

    #[test]
    fn cp2_rename_uses_the_existing_file_identity_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let first = commit(dir, "a.rs", "make a\n\nImplements: c-AAAAA\n");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git(dir, &["mv", "a.rs", "src/b.rs"]);
        git(dir, &["commit", "-q", "--no-verify", "-m", "rename a to b"]);
        let m = flat();
        let ev = evidence(dir);
        assert_eq!(
            files(anchored_files(&m, &id(X), &ev)),
            ["src/b.rs"],
            "the citation follows the file to its new path; the old path is gone"
        );
        assert!(!later_touch(&ev, Some(&first), "src/b.rs", &allowed(&[X]), &m).unwrap());
        assert!(!later_touch(&ev, Some(&first), "a.rs", &allowed(&[X]), &m).unwrap());
        commit(dir, "src/b.rs", "touch b\n\nImplements: c-AAAAA");
        let ev = evidence(dir);
        assert!(later_touch(&ev, Some(&first), "src/b.rs", &allowed(&[X]), &m).unwrap());
        assert!(!later_touch(&ev, Some(&first), "a.rs", &allowed(&[X]), &m).unwrap());
    }

    #[test]
    fn cp2_blob_state_ignores_dirty_and_staged_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        commit(dir, "a.rs", "a");
        std::fs::create_dir_all(dir.join("deep/er")).unwrap();
        std::fs::write(dir.join("deep/er/run.sh"), "#!/bin/sh\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["update-index", "--chmod=+x", "deep/er/run.sh"]);
        let blob = git(dir, &["rev-parse", ":a.rs"]);
        git(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{blob},link"),
            ],
        );
        let head = git(dir, &["rev-parse", "HEAD"]);
        git(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},sub"),
            ],
        );
        git(dir, &["commit", "-q", "--no-verify", "-m", "more files"]);
        let before = evidence(dir);
        let paths: Vec<&str> = before.blobs.keys().map(String::as_str).collect();
        assert_eq!(
            paths,
            ["a.rs", "deep/er/run.sh", "link"],
            "regular, executable and symlink files; no directory, no submodule"
        );
        assert_eq!(before.blobs["a.rs"], git(dir, &["rev-parse", "HEAD:a.rs"]));
        assert_eq!(before.blobs["link"], blob);
        // dirty, staged, untracked and removed files
        std::fs::write(dir.join("a.rs"), "changed\n").unwrap();
        std::fs::write(dir.join("new.rs"), "new\n").unwrap();
        std::fs::write(dir.join("deep/er/run.sh"), "staged\n").unwrap();
        git(dir, &["add", "deep/er/run.sh"]);
        std::fs::remove_file(dir.join("link")).ok();
        let after = evidence(dir);
        assert_eq!(before.blobs, after.blobs);
        assert_eq!(before, after);
    }

    #[test]
    fn cp2_a_captured_head_survives_a_later_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let c0 = commit(dir, "f.rs", "zero");
        let c1 = commit(dir, "f.rs", "one\n\nImplements: c-AAAAA");
        let m = flat();
        let seen = evidence(dir);
        assert_eq!(seen.head.as_deref(), Some(c1.as_str()));
        let c2 = commit(dir, "f.rs", "two\n\nImplements: c-AAAAA");
        commit(dir, "extra.rs", "extra");
        assert_eq!(seen.head.as_deref(), Some(c1.as_str()));
        assert_eq!(files(seen.blobs.keys().cloned().collect()), ["f.rs"]);
        assert!(later_touch(&seen, Some(&c0), "f.rs", &allowed(&[X]), &m).unwrap());
        assert!(
            !later_touch(&seen, Some(&c1), "f.rs", &allowed(&[X]), &m).unwrap(),
            "the commit made after the capture is not later than the captured HEAD"
        );
        let now = evidence(dir);
        assert_ne!(now, seen);
        assert!(later_touch(&now, Some(&c1), "f.rs", &allowed(&[X]), &m).unwrap());
        // the evidence at the recorded HEAD is the one captured then
        let again = at(&repo_of(dir), Some(&c1)).unwrap();
        assert_eq!(again, seen);
        assert_ne!(again.head.as_deref(), Some(c2.as_str()));
    }

    #[test]
    fn cp2_unborn_and_missing_old_commit_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let m = flat();
        let empty = evidence(dir);
        assert_eq!(empty.head, None);
        assert!(empty.blobs.is_empty() && empty.index.files.is_empty());
        assert!(anchored_files(&m, &id(X), &empty).is_empty());
        assert!(!later_touch(&empty, None, "f.rs", &allowed(&[X]), &m).unwrap());
        let c1 = commit(dir, "f.rs", "one\n\nImplements: c-AAAAA");
        let ev = evidence(dir);
        assert!(later_touch(&ev, None, "f.rs", &allowed(&[X]), &m).unwrap());
        // a first HEAD this checkout does not hold closes nothing, and nothing is fetched
        let missing = "0123456789abcdef0123456789abcdef01234567";
        assert!(!later_touch(&ev, Some(missing), "f.rs", &allowed(&[X]), &m).unwrap());
        assert!(!later_touch(&ev, Some("not a hash"), "f.rs", &allowed(&[X]), &m).unwrap());
        // evidence at a recorded HEAD the checkout does not hold is empty and marked so
        let gone = at(&repo_of(dir), Some(missing)).unwrap();
        assert!(!gone.available && gone.blobs.is_empty());
        assert_eq!(gone.head.as_deref(), Some(missing));
        assert!(!later_touch(&gone, None, "f.rs", &allowed(&[X]), &m).unwrap());
        assert!(at(&repo_of(dir), Some(&c1)).unwrap().available);
        // a configured folder that is no repository keeps the configuration refusal
        let plain = tempfile::tempdir().unwrap();
        let err = at(&repo_of(plain.path()), None).unwrap_err();
        assert_eq!(err.code, E_CONFIG);
    }

    #[test]
    fn cp2_capture_lists_the_linked_repos_by_name() {
        let project = tempfile::tempdir().unwrap();
        let (a, z) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for (d, f) in [(a.path(), "a.rs"), (z.path(), "z.rs")] {
            init(d);
            commit(d, f, "x");
        }
        std::fs::write(
            project.path().join(".rdy.yaml"),
            "code_repos: [zeta, alpha]\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join(".rdy")).unwrap();
        std::fs::write(
            project.path().join(".rdy/local.yaml"),
            format!(
                "code_paths:\n  alpha: {}\n  zeta: {}\n",
                a.path().display(),
                z.path().display()
            ),
        )
        .unwrap();
        let got = capture_with(project.path(), None).unwrap();
        let names: Vec<&str> = got.iter().map(|e| e.repo.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(files(got[0].blobs.keys().cloned().collect()), ["a.rs"]);
        assert_eq!(files(got[1].blobs.keys().cloned().collect()), ["z.rs"]);
        assert_eq!(
            got[0].head.as_deref(),
            Some(git(a.path(), &["rev-parse", "HEAD"]).as_str())
        );
    }

    #[test]
    fn cp2_git_output_bounds_join_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        // a blob larger than a pipe holds: git blocks writing it unless the reader keeps up
        std::fs::write(dir.join("big.bin"), vec![b'x'; 1 << 20]).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--no-verify", "-m", "big"]);
        let repo = repo_of(dir);
        let reaped = || REAPED.with(std::cell::Cell::get);

        let before = reaped();
        let mut c = crate::code_index::git_command(&repo.root);
        c.args(["cat-file", "blob", "HEAD:big.bin"]);
        let lim = Limits {
            stdout: 1024,
            stderr: MAX_GIT_ERROR,
        };
        let err = run_with(c, lim).unwrap_err();
        assert_eq!(err.code, E_LIMIT);
        assert_eq!(err.message, "code evidence exceeds 1024 output bytes");
        assert_eq!(err.fix, "use a smaller linked checkout");
        assert_eq!(
            reaped(),
            before + 1,
            "the killed child was waited for before the return"
        );

        // a bound that holds returns the whole output
        let mut c = crate::code_index::git_command(&repo.root);
        c.args(["cat-file", "blob", "HEAD:big.bin"]);
        let lim = Limits {
            stdout: 1 << 20,
            stderr: MAX_GIT_ERROR,
        };
        assert_eq!(run_with(c, lim).unwrap().stdout.len(), 1 << 20);

        // stderr has its own bound
        let before = reaped();
        let mut c = crate::code_index::git_command(&repo.root);
        c.args([
            "cat-file",
            "blob",
            "0123456789abcdef0123456789abcdef01234567",
        ]);
        let lim = Limits {
            stdout: MAX_GIT_OUTPUT,
            stderr: 8,
        };
        let err = run_with(c, lim).unwrap_err();
        assert_eq!(err.message, "code evidence exceeds 8 error bytes");
        assert_eq!(reaped(), before + 1);

        // the reader itself: an endless source stops at the bound, and a refused read is an error
        assert_eq!(drain(std::io::repeat(7), 100).unwrap(), Drained::Over);
        assert_eq!(
            drain(&[1u8, 2, 3][..], 3).unwrap(),
            Drained::Done(vec![1, 2, 3])
        );
        assert_eq!(drain(&[1u8, 2, 3, 4][..], 3).unwrap(), Drained::Over);

        // tree bounds: paths and path bytes
        let entry = |path: &str| format!("100644 blob {}\t{path}\0", "a".repeat(40)).into_bytes();
        let two = [entry("a"), entry("b")].concat();
        assert_eq!(parse_tree(&two, 2, 8).unwrap().len(), 2);
        let err = parse_tree(&two, 1, 8).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (E_LIMIT, "code evidence exceeds 1 paths")
        );
        let long = entry(&"p".repeat(9));
        let err = parse_tree(&long, 2, 8).unwrap_err();
        assert_eq!(err.message, "code evidence exceeds 8 path bytes");
        assert_eq!((MAX_GIT_OUTPUT, MAX_GIT_ERROR), (268_435_456, 65_536));
        assert_eq!((MAX_TREE_PATHS, MAX_TREE_PATH), (1_000_000, 4096));
    }
}
