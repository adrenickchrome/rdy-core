//! A throwaway git repository for the code-index tests (cidx1, cbrief1, prov1, cmcp1, ccli1,
//! cperf1): fixed dates, no hooks, no signing, whatever the machine's git config says.
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitRepo {
    dir: PathBuf,
    _tmp: Option<tempfile::TempDir>,
}

/// `n` numbered lines tagged `tag`: big enough that git's rename detection (50% similarity)
/// pairs a renamed file that also gained a line.
pub fn lines(n: usize, tag: &str) -> String {
    (0..n).map(|i| format!("{tag} line {i}\n")).collect()
}

/// Appends `text` to the project's `.rdy.yaml`.
pub fn append_config(project: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(project.join(".rdy.yaml"))
        .expect("open the project config");
    f.write_all(text.as_bytes())
        .expect("append to the project config");
}

/// Appends `code_repos:` listing `paths` (each a quoted YAML string) to the project's config.
pub fn link_repos(project: &Path, paths: &[&str]) {
    let mut text = String::from("code_repos:\n");
    for p in paths {
        text.push_str(&format!("  - {}\n", serde_json::to_string(p).unwrap()));
    }
    append_config(project, &text);
}

impl GitRepo {
    /// A new repository on branch `main` in a fresh temporary directory.
    pub fn new() -> GitRepo {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = GitRepo {
            dir: tmp.path().to_path_buf(),
            _tmp: Some(tmp),
        };
        g.run(&["init", "-q", "--initial-branch=main"]);
        g
    }

    /// A new repository on branch `main` with `commits` linear commits over `files` files
    /// (`src/f{k}.rs`), built in one `git fast-import` run. Commit `c` touches two files and
    /// cites `cite[c % n]` and `cite[(c + 1) % n]` in an `Implements:` trailer; `cite` must not
    /// be empty. For the perf ops: a real history at a size no per-commit `git commit` loop
    /// could build in reasonable time.
    pub fn synthetic(commits: usize, files: usize, cite: &[String]) -> GitRepo {
        use std::process::Stdio;
        assert!(!cite.is_empty(), "synthetic needs at least one id to cite");
        let g = GitRepo::new();
        let n = cite.len();
        let mut s: Vec<u8> = Vec::new();
        for c in 0..commits {
            let msg = format!(
                "change {c}\n\nImplements: {}, {}\n",
                cite[c % n],
                cite[(c + 1) % n]
            );
            s.extend_from_slice(b"commit refs/heads/main\n");
            s.extend_from_slice(format!("mark :{}\n", c + 1).as_bytes());
            s.extend_from_slice(b"committer qa <qa> 1767225600 +0000\n");
            s.extend_from_slice(format!("data {}\n{msg}", msg.len()).as_bytes());
            if c > 0 {
                s.extend_from_slice(format!("from :{c}\n").as_bytes());
            }
            let first = (c * 7) % files;
            let second = (c * 13 + 1) % files;
            let content = format!("// commit {c}\n").repeat(20);
            for k in [Some(first), (second != first).then_some(second)]
                .into_iter()
                .flatten()
            {
                s.extend_from_slice(format!("M 100644 inline src/f{k}.rs\n").as_bytes());
                s.extend_from_slice(format!("data {}\n{content}", content.len()).as_bytes());
            }
            s.push(b'\n');
        }
        let mut child = g
            .git()
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn git fast-import");
        // a writer thread: the stream can outgrow the pipe buffer while git is still reading
        let mut stdin = child.stdin.take().expect("fast-import stdin");
        let writer = std::thread::spawn(move || stdin.write_all(&s));
        let out = child.wait_with_output().expect("wait for git fast-import");
        writer
            .join()
            .expect("writer thread")
            .expect("feed git fast-import");
        assert!(
            out.status.success(),
            "git fast-import: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        g.run(&["reset", "-q", "--hard", "main"]);
        g
    }

    /// A new repository on branch `main` at `dir` (created if missing); removed with its parent.
    pub fn at(dir: &Path) -> GitRepo {
        std::fs::create_dir_all(dir).expect("mkdir");
        let g = GitRepo {
            dir: dir.to_path_buf(),
            _tmp: None,
        };
        g.run(&["init", "-q", "--initial-branch=main"]);
        g
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// This repository's path as `link` writes it into `code_repos`.
    pub fn configured(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }

    fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(&self.dir)
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
        c
    }

    /// Runs git with `args`, panicking with its stderr on failure; returns trimmed stdout.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self.git().args(args).output().expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn write(&self, path: &str, text: &str) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }

    pub fn mv(&self, from: &str, to: &str) {
        std::fs::create_dir_all(self.dir.join(to).parent().unwrap()).expect("mkdir");
        self.run(&["mv", from, to]);
    }

    pub fn rm(&self, path: &str) {
        self.run(&["rm", "-q", path]);
    }

    /// Stages everything and commits with `message`; returns the new HEAD's full hash.
    pub fn commit(&self, message: &str) -> String {
        self.run(&["add", "-A"]);
        self.run(&[
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            message,
        ]);
        self.run(&["rev-parse", "HEAD"])
    }

    /// Appends `code_repos: [<this repository>]` to the project's `.rdy.yaml`.
    pub fn link(&self, project: &Path) {
        link_repos(project, &[&self.configured()]);
    }
}

impl Default for GitRepo {
    fn default() -> Self {
        GitRepo::new()
    }
}
