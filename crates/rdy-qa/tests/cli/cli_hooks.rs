//! m4a3 (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1; D-KG0V6; D-EKGN9;
//! D-PHH2Q): `rdy setup` with its two optional hook flags, the `rdy hook` commands and their two
//! host entry points. Bare setup keeps its result. `--code-hook <repo>` installs the Git
//! `commit-msg` warning and `--edit-hook claude-code` the Claude Code `PostToolUse` adapter, each
//! only when asked, after the existing steps. `rdy hook run-code` reads the actual staged diff
//! (an alternate `GIT_INDEX_FILE` too) of the caller's checkout, at its HEAD, and `rdy hook
//! run-edit` reads one host event from stdin; both warn and exit 0. The daemon and a direct
//! process give the same results, and public usage errors keep their exit values.
//!
//! Every process has a home and a user config of its own ([`World`]), and every Git run has no
//! system or global configuration of the machine. Each checkout names its own `.git/hooks` in its
//! local config, so no machine-wide `core.hooksPath` applies.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within_stdin;
use rdy_qa::instructions::{Dir, World};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const NO_CITATION: &str =
    "This commit touches code anchored to a decision and has no Implements trailer.";
const DIRECT_EDIT: &str = "An agent edited rdy working-copy files directly.";
const DIRECT_EDIT_FIX: &str =
    "Read and write through rdy's tools. Call rdy_instructions and load rdy:write.";

const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_session, then rdy_instructions, and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

/// The 0.43.0 to 0.45.0 stub.
const OLD_STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

/// The Git variables a hook inherits from Git: none reaches a fixture's own Git or rdy run unless
/// the test sets it.
const GIT_VARS: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
];

/// `cmd` with no system Git configuration and the empty global configuration `global`.
fn git_isolated<'a>(cmd: &'a mut Command, global: &Path) -> &'a mut Command {
    for v in GIT_VARS {
        cmd.env_remove(v);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
}

/// Git in `dir`, as the fixture's own author, with no configuration of the machine.
fn git_command(dir: &Path, global: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args([
        "-c",
        "user.name=qa",
        "-c",
        "user.email=qa@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ]);
    c.args(args);
    git_isolated(&mut c, global);
    c
}

fn git(dir: &Path, global: &Path, args: &[&str]) -> String {
    let out = git_command(dir, global, args).output().expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project of a [`World`] with a landed decision, and two linked code repos: `app`, whose
/// history anchors `src/anchored.rs` to the decision and leaves `src/plain.rs` unanchored, and
/// `fresh`, which has no commit yet. Each checkout's hooks folder is its own `.git/hooks`.
struct Hooked {
    w: World,
    project: Dir,
    tmp: tempfile::TempDir,
    /// The canonical root of `app`.
    app: PathBuf,
    /// The canonical root of `fresh`.
    fresh: PathBuf,
    decision: String,
    /// The mode of the last rdy run: `Some(true)` direct, with the daemon stopped, since a
    /// direct run cannot open a store the daemon holds.
    mode: Cell<Option<bool>>,
}

impl Hooked {
    fn new() -> Hooked {
        let w = World::new();
        let project = w.project();
        let decision = seed_decision(project.path());
        let tmp = short_tempdir();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("no-global"), "").unwrap();
        let app = base.join("app co");
        let fresh = base.join("fresh");
        let h = Hooked {
            w,
            project,
            tmp,
            app,
            fresh,
            decision,
            mode: Cell::new(None),
        };
        for dir in [&h.app, &h.fresh] {
            std::fs::create_dir_all(dir).unwrap();
            h.git(dir, &["init", "-q", "--initial-branch=main"]);
            let hooks = dir.join(".git").join("hooks");
            h.git(
                dir,
                &["config", "core.hooksPath", &hooks.display().to_string()],
            );
        }
        h.write(&h.app, "src/plain.rs", "fn plain() {}\n");
        h.commit_all(&h.app, "feat: plain");
        h.write(&h.app, "src/anchored.rs", "fn anchored() {}\n");
        h.commit_all(
            &h.app,
            &format!("feat: anchored\n\nImplements: {}", h.decision),
        );
        rdy_qa::git_repo::link_repos(
            h.project.path(),
            &[
                ("app", &h.app.display().to_string()),
                ("fresh", &h.fresh.display().to_string()),
            ],
        );
        h
    }

    /// The empty global configuration. Not under the canonical root: on Windows that is a `\\?\`
    /// path, which Git cannot open.
    fn global(&self) -> PathBuf {
        self.tmp.path().join("no-global")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        git(dir, &self.global(), args)
    }

    fn write(&self, dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Stages everything in `dir` and commits it with no hook.
    fn commit_all(&self, dir: &Path, message: &str) -> String {
        self.git(dir, &["add", "-A"]);
        self.git(dir, &["commit", "-q", "--no-verify", "-m", message]);
        self.git(dir, &["rev-parse", "HEAD"])
    }

    /// Puts `dir` back at its HEAD: nothing staged, nothing changed.
    fn clean(&self, dir: &Path) {
        self.git(dir, &["reset", "-q", "--hard"]);
        self.git(dir, &["clean", "-qfd"]);
    }

    /// `rdy --project <project> <args>` in `cwd` as the world's human, through the daemon or
    /// in this process (`direct`), with `env` added.
    fn rdy_command(
        &self,
        cwd: &Path,
        direct: bool,
        env: &[(&str, &Path)],
        args: &[&str],
    ) -> Command {
        if self.mode.get() != Some(direct) {
            if direct {
                self.w.stop(self.project.path());
            } else {
                self.start_daemon();
            }
            self.mode.set(Some(direct));
        }
        let mut cmd = self.w.command(self.project.path(), args);
        cmd.current_dir(cwd);
        git_isolated(&mut cmd, &self.global());
        if direct {
            cmd.env("RDY_NO_DAEMON", "1");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    fn rdy_in(&self, cwd: &Path, direct: bool, env: &[(&str, &Path)], args: &[&str]) -> Output {
        self.w
            .run(&mut self.rdy_command(cwd, direct, env, args))
            .expect("run rdy")
    }

    /// The JSON on stdout of `rdy --json <args>`, which must exit 0.
    fn ok_json(&self, cwd: &Path, direct: bool, env: &[(&str, &Path)], args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.rdy_in(cwd, direct, env, &all);
        assert_eq!(
            out.status.code(),
            Some(0),
            "rdy {all:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("rdy {all:?}: {e}: {text}"))
    }

    /// `rdy hook run-code --repo <repo>` in `cwd` on a message file holding `message`.
    fn run_code(
        &self,
        cwd: &Path,
        direct: bool,
        repo: &str,
        message: &str,
        index: Option<&Path>,
    ) -> Value {
        let file = self.tmp.path().join("MSG ü 'x'");
        std::fs::write(&file, message).unwrap();
        let env: Vec<(&str, &Path)> = index.map(|i| ("GIT_INDEX_FILE", i)).into_iter().collect();
        let v = self.ok_json(
            cwd,
            direct,
            &env,
            &[
                "hook",
                "run-code",
                "--repo",
                repo,
                "--message-file",
                file.to_str().unwrap(),
            ],
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            message,
            "the message stays"
        );
        v
    }

    /// Starts the project's daemon, so a hook run's deadline never covers a daemon start.
    fn start_daemon(&self) {
        let out = self.w.rdy(self.project.path(), &["status"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Lands a doc with one section holding one decision in the project at `dir`; returns the
/// decision's id.
fn seed_decision(dir: &Path) -> String {
    use rdy_qa::fixtures::{land, write, writer};
    let mut r = rdy_engine::Rdy::open(dir).expect("open the project");
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
        ]),
    )
    .expect("seed the doc");
    let d = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: w.created["$s"].clone(),
                statement: "Anchored code cites its decision".into(),
                ..Default::default()
            },
        )
        .expect("decide");
    let decision = d.created["$decision"].clone();
    land(&mut r, "work").expect("land the seed");
    r.close().expect("close");
    decision
}

/// The one warning of a check, by code, with the paths it names.
fn warnings(v: &Value) -> Vec<(String, Vec<String>)> {
    v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("a check result: {v}"))
        .iter()
        .map(|w| {
            let paths = w["paths"]
                .as_array()
                .map(|p| p.iter().map(|s| s.as_str().unwrap().to_string()).collect())
                .unwrap_or_default();
            (w["code"].as_str().unwrap().to_string(), paths)
        })
        .collect()
}

/// A clean code check.
fn clean(v: &Value) {
    assert_eq!(v["kind"], "code", "{v}");
    assert_eq!(v["checked"], true, "{v}");
    assert_eq!(v["warnings"], json!([]), "{v}");
}

/// A code check that warns about exactly `paths`.
fn warns(v: &Value, paths: &[&str]) {
    assert_eq!(v["kind"], "code", "{v}");
    assert_eq!(v["checked"], true, "{v}");
    let want: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();
    assert_eq!(
        warnings(v),
        vec![("W_HOOK_NO_CITATION".to_string(), want)],
        "{v}"
    );
    assert_eq!(v["warnings"][0]["message"], NO_CITATION, "{v}");
}

/// An unavailable check for `reason`.
fn unavailable(v: &Value, reason: &str) {
    assert_eq!(v["checked"], false, "{v}");
    assert_eq!(v["warnings"][0]["code"], "W_HOOK_UNAVAILABLE", "{v}");
    assert_eq!(v["warnings"][0]["reason"], reason, "{v}");
    assert_eq!(
        v["warnings"][0]["message"],
        format!("The rdy hook could not check this action: {reason}."),
        "{v}"
    );
}

#[test]
fn m4a3_git_runner_uses_the_actual_index() {
    let h = Hooked::new();
    h.start_daemon();
    let app = h.app.clone();
    let plain = "feat: change\n";
    let cited = format!("feat: change\n\nImplements: {}\n", h.decision);
    for direct in [false, true] {
        // partial staging: the anchored file is changed but only the plain one is staged
        h.write(&app, "src/plain.rs", "fn plain() { 1; }\n");
        h.write(&app, "src/anchored.rs", "fn anchored() { 1; }\n");
        h.git(&app, &["add", "src/plain.rs"]);
        clean(&h.run_code(&app, direct, "app", plain, None));
        // the anchored file staged: a warning, and a citation clears it
        h.git(&app, &["add", "src/anchored.rs"]);
        warns(
            &h.run_code(&app, direct, "app", plain, None),
            &["src/anchored.rs"],
        );
        clean(&h.run_code(&app, direct, "app", &cited, None));
        h.clean(&app);

        // an alternate index: it holds the anchored change, the default index holds nothing
        let alt = h.tmp.path().join("alt index");
        let _ = std::fs::remove_file(&alt);
        let mut read_tree = git_command(&app, &h.global(), &["read-tree", "HEAD"]);
        read_tree.env("GIT_INDEX_FILE", &alt);
        assert!(read_tree.status().unwrap().success());
        h.write(&app, "src/anchored.rs", "fn anchored() { 2; }\n");
        let mut add = git_command(&app, &h.global(), &["add", "src/anchored.rs"]);
        add.env("GIT_INDEX_FILE", &alt);
        assert!(add.status().unwrap().success());
        warns(
            &h.run_code(&app, direct, "app", plain, Some(&alt)),
            &["src/anchored.rs"],
        );
        clean(&h.run_code(&app, direct, "app", plain, None));
        h.clean(&app);

        // a deletion, and both sides of a rename
        h.git(&app, &["rm", "-q", "src/anchored.rs"]);
        warns(
            &h.run_code(&app, direct, "app", plain, None),
            &["src/anchored.rs"],
        );
        h.clean(&app);
        h.git(&app, &["mv", "src/anchored.rs", "src/moved.rs"]);
        warns(
            &h.run_code(&app, direct, "app", plain, None),
            &["src/anchored.rs"],
        );
        h.clean(&app);

        // a checkout that is not the linked repo or one of its worktrees
        let other = h.tmp.path().join(format!("other {direct}"));
        std::fs::create_dir_all(&other).unwrap();
        h.git(&other, &["init", "-q", "--initial-branch=main"]);
        h.write(&other, "src/anchored.rs", "fn other() {}\n");
        h.git(&other, &["add", "-A"]);
        unavailable(
            &h.run_code(&other, direct, "app", plain, None),
            "checkout unavailable",
        );

        // an unborn HEAD: a verified fresh repository is clean
        h.write(&h.fresh, "src/new.rs", "fn new() {}\n");
        h.git(&h.fresh, &["add", "-A"]);
        let fresh = h.fresh.clone();
        clean(&h.run_code(&fresh, direct, "fresh", plain, None));
    }

    // divergent worktree histories: each branch anchors its own file
    let base = h.git(&app, &["rev-parse", "HEAD"]);
    h.write(&app, "src/feature.rs", "fn feature() {}\n");
    h.write(&app, "src/onmain.rs", "fn onmain() {}\n");
    let base_two = h.commit_all(&app, "feat: two more files");
    assert_ne!(base, base_two);
    let wt = std::fs::canonicalize(h.tmp.path()).unwrap().join("wt ü");
    // Git makes the folder: it gets the path without a `\\?\` prefix, which Git cannot create
    h.git(
        &app,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            &rdy_store::pio::display_path(&wt),
        ],
    );
    h.write(&wt, "src/feature.rs", "fn feature() { 1; }\n");
    h.commit_all(&wt, &format!("feat: feature\n\nImplements: {}", h.decision));
    h.write(&app, "src/onmain.rs", "fn onmain() { 1; }\n");
    h.commit_all(&app, &format!("feat: onmain\n\nImplements: {}", h.decision));
    for direct in [false, true] {
        for (dir, file, anchored) in [
            (&wt, "src/feature.rs", true),
            (&wt, "src/onmain.rs", false),
            (&app, "src/feature.rs", false),
            (&app, "src/onmain.rs", true),
        ] {
            h.write(dir, file, "fn changed() {}\n");
            h.git(dir, &["add", file]);
            let v = h.run_code(dir, direct, "app", plain, None);
            if anchored {
                warns(&v, &[file]);
            } else {
                clean(&v);
            }
            h.clean(dir);
        }
    }

    // a changed HEAD: the check at a HEAD the checkout no longer has
    let input = h.tmp.path().join("stale.json");
    std::fs::write(
        &input,
        json!({"repo": "app", "checkout": app.to_str().unwrap(), "head": base,
               "paths": ["src/anchored.rs"], "message": plain})
        .to_string(),
    )
    .unwrap();
    for direct in [false, true] {
        let v = h.ok_json(
            &app,
            direct,
            &[],
            &[
                "hook",
                "check",
                "--kind",
                "code",
                "--input",
                input.to_str().unwrap(),
            ],
        );
        unavailable(&v, "history unavailable");
    }
}

/// Whether this host runs Git's shell hooks in these tests: Git for Windows runs the launcher
/// under its own shell, which the native Windows gate proves with a real commit.
fn shell_hooks_run_here() -> bool {
    if cfg!(windows) {
        eprintln!("skip: the launcher runs under Git for Windows' shell in the native gate");
        return false;
    }
    true
}

#[test]
fn m4a3_git_runner_keeps_message_and_commit() {
    let h = Hooked::new();
    h.start_daemon();
    let app = h.app.clone();
    // a host run that cannot read its message still warns and exits 0
    let missing = h.tmp.path().join("no such message");
    for direct in [false, true] {
        let out = h.rdy_in(
            &app,
            direct,
            &[],
            &[
                "--json",
                "hook",
                "run-code",
                "--repo",
                "app",
                "--message-file",
                missing.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(0));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        unavailable(&v, "I/O failure");
        // text mode: the warning goes to stderr, nothing to stdout
        let out = h.rdy_in(
            &app,
            direct,
            &[],
            &[
                "hook",
                "run-code",
                "--repo",
                "app",
                "--message-file",
                missing.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(0));
        assert!(
            out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("W_HOOK_UNAVAILABLE"), "{err}");
    }
    if !shell_hooks_run_here() {
        return;
    }
    let installed = h.ok_json(
        &app,
        false,
        &[],
        &["hook", "install", "--kind", "code", "--repo", "app"],
    );
    assert_eq!(installed["files"][0]["result"], "written", "{installed}");
    let message = "feat: keep me\n\nBody with ü and 'quotes'.\n";
    let file = h.tmp.path().join("commit message");
    std::fs::write(&file, message).unwrap();
    // a real commit: the warning prints, and the commit and its message bytes stand
    let commit = |label: &str| -> Output {
        h.write(
            &app,
            "src/anchored.rs",
            &format!("fn anchored() {{ \"{label}\"; }}\n"),
        );
        h.git(&app, &["add", "src/anchored.rs"]);
        let mut cmd = git_command(
            &app,
            &h.global(),
            &["commit", "-q", "-F", file.to_str().unwrap()],
        );
        h.w.isolate(&mut cmd);
        cmd.env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON");
        let before = h.git(&app, &["rev-parse", "HEAD"]);
        let out = h.w.run(&mut cmd).expect("git commit");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(
            h.git(&app, &["rev-parse", "HEAD"]),
            before,
            "the commit was made"
        );
        let raw = h.git(&app, &["cat-file", "commit", "HEAD"]);
        let body = raw.split_once("\n\n").unwrap().1;
        assert_eq!(format!("{body}\n"), message, "the message bytes stand");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), message);
        out
    };
    let out = commit("warned");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("W_HOOK_NO_CITATION"), "{err}");
    assert!(err.contains(NO_CITATION), "{err}");
    // the project unavailable: the commit still goes ahead, with the unavailable warning
    h.w.stop(h.project.path());
    let cfg = h.project.path().join(".rdy.yaml");
    let aside = h.project.path().join(".rdy.yaml.aside");
    std::fs::rename(&cfg, &aside).unwrap();
    let out = commit("unavailable");
    std::fs::rename(&aside, &cfg).unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("W_HOOK_UNAVAILABLE"), "{err}");
    assert!(err.contains("project unavailable"), "{err}");
}

/// A Claude Code `PostToolUse` envelope for `tool` on `path`, with the fields a real one carries
/// and that the adapter must not pass on.
fn envelope(tool: &str, path: &str) -> Value {
    json!({
        "session_id": "s-1",
        "transcript_path": "/home/someone/.claude/projects/x/transcript-secret.jsonl",
        "cwd": "/somewhere",
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": {"file_path": path, "old_string": "old-secret-text", "new_string": "new-secret-text", "content": "body-secret-text"},
        "tool_response": {"filePath": path, "success": true, "originalFile": "original-secret-text"}
    })
}

/// `rdy --project <project> hook run-edit --harness claude-code` with `stdin`.
fn run_edit(h: &Hooked, direct: bool, stdin: &[u8]) -> Output {
    let mut cmd = h.rdy_command(
        h.tmp.path(),
        direct,
        &[],
        &["hook", "run-edit", "--harness", "claude-code"],
    );
    output_within_stdin(&mut cmd, Some(stdin), Duration::from_secs(120)).expect("run rdy")
}

/// The additional context of a host reply, which carries only that and the event name.
fn context_of(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["hookSpecificOutput"], "{v}");
    let inner = &v["hookSpecificOutput"];
    let keys: Vec<&String> = inner.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["additionalContext", "hookEventName"], "{v}");
    assert_eq!(inner["hookEventName"], "PostToolUse", "{v}");
    let text = inner["additionalContext"].as_str().unwrap().to_string();
    for secret in ["secret", "transcript"] {
        assert!(!text.contains(secret), "{text}");
    }
    text
}

#[test]
fn m4a3_edit_runner_emits_only_warning_context() {
    let h = Hooked::new();
    let out = h.w.rdy(h.project.path(), &["checkout"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let root = std::fs::canonicalize(h.project.path()).unwrap();
    let doc = root.join("spec.yaml");
    assert!(doc.is_file(), "the checkout wrote the doc");
    let code = h.app.join("src/plain.rs");
    for direct in [false, true] {
        // a working-copy member: the warning, as context only
        for tool in ["Edit", "Write"] {
            let event = envelope(tool, doc.to_str().unwrap()).to_string();
            let text = context_of(&run_edit(&h, direct, event.as_bytes()));
            assert!(text.contains("W_HOOK_DIRECT_EDIT"), "{text}");
            assert!(text.contains(DIRECT_EDIT), "{text}");
            assert!(text.contains(DIRECT_EDIT_FIX), "{text}");
            assert!(text.contains(&rdy_store::pio::display_path(&doc)), "{text}");
        }
        // a normal code file: no output at all
        let out = run_edit(
            &h,
            direct,
            envelope("Write", code.to_str().unwrap())
                .to_string()
                .as_bytes(),
        );
        assert_eq!(out.status.code(), Some(0));
        assert!(
            out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        // an event that is no Edit or Write result, or no JSON: unavailable, still exit 0
        for bad in [
            b"not json".to_vec(),
            envelope("Bash", doc.to_str().unwrap()).to_string().into_bytes(),
            json!({"hook_event_name": "PreToolUse", "tool_name": "Edit", "tool_input": {"file_path": doc}}).to_string().into_bytes(),
            json!({"hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {}}).to_string().into_bytes(),
            json!({"hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {"file_path": "relative/spec.yaml"}}).to_string().into_bytes(),
        ] {
            let text = context_of(&run_edit(&h, direct, &bad));
            assert!(text.contains("W_HOOK_UNAVAILABLE"), "{text}");
            assert!(text.contains("invalid event"), "{text}");
        }
        // more than the input bound
        let big = vec![b' '; rdy_engine::hooks::MAX_HOOK_INPUT + 1];
        let text = context_of(&run_edit(&h, direct, &big));
        assert!(text.contains("input limit"), "{text}");
    }
}

#[test]
fn m4a3_cli_and_direct_mode_share_hook_results() {
    let h = Hooked::new();
    h.start_daemon();
    let app = h.app.clone();
    let head = h.git(&app, &["rev-parse", "HEAD"]);
    let input = |name: &str, v: Value| -> PathBuf {
        let p = h.tmp.path().join(name);
        std::fs::write(&p, v.to_string()).unwrap();
        p
    };
    let checkout = app.to_str().unwrap();
    let warned = input(
        "warned.json",
        json!({"repo": "app", "checkout": checkout, "head": head,
               "paths": ["src/anchored.rs", "src/plain.rs"], "message": "feat: x\n"}),
    );
    let cited = input(
        "cited.json",
        json!({"repo": "app", "checkout": checkout, "head": head,
               "paths": ["src/anchored.rs"], "message": format!("feat: x\n\nimplements: {}\n", h.decision)}),
    );
    let root = std::fs::canonicalize(h.project.path()).unwrap();
    let edit = input(
        "edit.json",
        json!({"paths": [root.join(".rdy.yaml").to_str().unwrap(), root.join("notes.md").to_str().unwrap()]}),
    );
    let check = |direct: bool, kind: &str, file: &Path| {
        h.ok_json(
            &app,
            direct,
            &[],
            &[
                "hook",
                "check",
                "--kind",
                kind,
                "--input",
                file.to_str().unwrap(),
            ],
        )
    };
    for (kind, file) in [("code", &warned), ("code", &cited), ("edit", &edit)] {
        let daemon = check(false, kind, file);
        let direct = check(true, kind, file);
        assert_eq!(daemon, direct, "{kind} {}", file.display());
        assert_eq!(daemon["kind"], kind, "{daemon}");
        assert_eq!(daemon["checked"], true, "{daemon}");
    }
    warns(&check(false, "code", &warned), &["src/anchored.rs"]);

    // install and remove: the same files through either path
    let edit_install = [
        "hook",
        "install",
        "--kind",
        "edit",
        "--harness",
        "claude-code",
    ];
    let edit_remove = [
        "hook",
        "remove",
        "--kind",
        "edit",
        "--harness",
        "claude-code",
    ];
    let settings = root.join(".claude").join("settings.local.json");
    let first = h.ok_json(&app, false, &[], &edit_install);
    assert_eq!(first["kind"], "edit", "{first}");
    assert_eq!(first["files"][0]["result"], "written", "{first}");
    assert!(settings.is_file());
    let again = h.ok_json(&app, true, &[], &edit_install);
    assert_eq!(again["files"][0]["result"], "kept", "{again}");
    assert_eq!(
        again["files"][0]["path"], first["files"][0]["path"],
        "{again}"
    );
    let removed = h.ok_json(&app, true, &[], &edit_remove);
    assert_eq!(removed["files"][0]["result"], "removed", "{removed}");
    assert!(!settings.exists());
    let code = h.ok_json(
        &app,
        true,
        &[],
        &["hook", "install", "--kind", "code", "--repo", "app"],
    );
    assert_eq!(code["files"][0]["result"], "written", "{code}");
    let code = h.ok_json(
        &app,
        false,
        &[],
        &["hook", "remove", "--kind", "code", "--repo", "app"],
    );
    assert_eq!(code["files"][0]["result"], "removed", "{code}");

    // public usage errors keep their exit values: the parser's 2, and a refused form's 1
    for direct in [false, true] {
        for args in [
            vec!["hook"],
            vec!["hook", "bogus"],
            vec![
                "hook",
                "check",
                "--kind",
                "other",
                "--input",
                warned.to_str().unwrap(),
            ],
            vec!["hook", "check", "--kind", "code"],
            vec!["hook", "install", "--repo", "app"],
            vec!["hook", "install", "--kind", "edit", "--harness", "other"],
            vec!["hook", "run-code", "--repo", "app"],
            vec!["hook", "run-edit"],
            vec!["setup", "--edit-hook", "other"],
        ] {
            let out = h.rdy_in(&app, direct, &[], &args);
            assert_eq!(
                out.status.code(),
                Some(2),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let refused = |args: &[&str], message: &str| {
            let mut all = vec!["--json"];
            all.extend_from_slice(args);
            let out = h.rdy_in(&app, direct, &[], &all);
            assert_eq!(out.status.code(), Some(1), "{all:?}");
            let v: Value = serde_json::from_slice(&out.stderr)
                .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
            assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
            assert_eq!(v["message"], message, "{v}");
            assert_eq!(
                v["fix"], "Use one documented check, install or remove form.",
                "{v}"
            );
        };
        refused(
            &["hook", "install", "--kind", "code"],
            "Invalid rdy_hook form: pass repo.",
        );
        refused(
            &[
                "hook",
                "install",
                "--kind",
                "code",
                "--repo",
                "app",
                "--harness",
                "claude-code",
            ],
            "Invalid rdy_hook form: remove harness: the install code form does not take it.",
        );
        let typo = input("typo.json", json!({"pathz": []}));
        refused(
            &[
                "hook",
                "check",
                "--kind",
                "edit",
                "--input",
                typo.to_str().unwrap(),
            ],
            "Invalid rdy_hook form: unknown field \"pathz\" (did you mean \"paths\"?).",
        );
        refused(
            &["hook", "install", "--kind", "code", "--repo", "ap"],
            "Invalid rdy_hook form: repo \"ap\" is no code_repos entry with a local checkout (did you mean \"app\"?).",
        );
        let big = h.tmp.path().join("big.json");
        std::fs::write(&big, vec![b' '; rdy_engine::hooks::MAX_HOOK_INPUT + 1]).unwrap();
        let out = h.rdy_in(
            &app,
            direct,
            &[],
            &[
                "--json",
                "hook",
                "check",
                "--kind",
                "edit",
                "--input",
                big.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(1));
        let v: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(v["code"], "E_LIMIT", "{v}");
        assert_eq!(v["message"], "The hook input exceeds 1048576 bytes.", "{v}");
    }
}

/// Every file under `dir` with its bytes, by path; nothing for a missing directory.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

/// `rdy <args>` in `cwd` with no `--project` unless `args` names one, as the world's human.
fn rdy_at(w: &World, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .current_dir(cwd);
    w.isolate(&mut cmd);
    w.run(&mut cmd).expect("rdy ran")
}

fn setup_json(w: &World, cwd: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json", "setup"];
    all.extend_from_slice(args);
    let out = rdy_at(w, cwd, &all);
    assert!(
        out.status.success(),
        "{all:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn m4a3_setup_installs_only_selected_hooks() {
    let commit_msg = |h: &Hooked| h.app.join(".git").join("hooks").join("commit-msg");
    let settings = |h: &Hooked| {
        std::fs::canonicalize(h.project.path())
            .unwrap()
            .join(".claude")
            .join("settings.local.json")
    };
    // what no option may touch: an unlinked repository, the home and the user config
    let untouched = |h: &Hooked, before: &[BTreeMap<PathBuf, Vec<u8>>]| {
        let now = [
            tree(&h.fresh.join(".git").join("hooks")),
            tree(h.w.home.path()),
            tree(&h.w.config_dir()),
        ];
        let meta = h.w.default_meta();
        assert!(tree(&meta.join(".claude")).is_empty());
        assert!(
            !now[1]
                .keys()
                .any(|p| p.starts_with(h.w.home.path().join(".claude")))
        );
        assert_eq!(now[0], before[0]);
        assert_eq!(now[2], before[2]);
    };
    let snapshot = |h: &Hooked| {
        vec![
            tree(&h.fresh.join(".git").join("hooks")),
            tree(h.w.home.path()),
            tree(&h.w.config_dir()),
        ]
    };
    let one = |v: &Value, kind: &str, result: &str, path: &Path| {
        let hooks = v["hooks"].as_array().unwrap_or_else(|| panic!("{v}"));
        let entry = hooks
            .iter()
            .find(|e| e["kind"] == kind)
            .unwrap_or_else(|| panic!("{kind}: {v}"));
        assert_eq!(
            entry["files"],
            json!([{"path": rdy_store::pio::display_path(path), "result": result}]),
            "{v}"
        );
        assert_eq!(entry["warnings"], json!([]), "{v}");
    };

    // the code hook alone
    let h = Hooked::new();
    let before = snapshot(&h);
    let v = setup_json(&h.w, h.project.path(), &["--code-hook", "app"]);
    assert_eq!(v["hooks"].as_array().unwrap().len(), 1, "{v}");
    one(&v, "code", "written", &commit_msg(&h));
    assert_eq!(v["stub"], json!({"result": "processed"}), "{v}");
    assert!(commit_msg(&h).is_file());
    assert!(!settings(&h).exists());
    assert!(!h.project.path().join(".claude").exists());
    untouched(&h, &before);

    // the edit hook alone
    let h = Hooked::new();
    let before = snapshot(&h);
    let v = setup_json(&h.w, h.project.path(), &["--edit-hook", "claude-code"]);
    assert_eq!(v["hooks"].as_array().unwrap().len(), 1, "{v}");
    one(&v, "edit", "written", &settings(&h));
    assert!(settings(&h).is_file());
    assert!(!commit_msg(&h).exists());
    untouched(&h, &before);

    // both, then again: written, then kept; the code hook first
    let h = Hooked::new();
    let before = snapshot(&h);
    let both = ["--code-hook", "app", "--edit-hook", "claude-code"];
    let v = setup_json(&h.w, h.project.path(), &both);
    let kinds: Vec<&str> = v["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["code", "edit"], "{v}");
    one(&v, "code", "written", &commit_msg(&h));
    one(&v, "edit", "written", &settings(&h));
    let v = setup_json(&h.w, h.project.path(), &both);
    one(&v, "code", "kept", &commit_msg(&h));
    one(&v, "edit", "kept", &settings(&h));
    untouched(&h, &before);
    // the text names each file after the existing lines
    let out = rdy_at(&h.w, h.project.path(), &["setup", "--code-hook", "app"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.trim_end().lines().last().unwrap();
    assert!(
        last.contains("kept") && last.contains("commit-msg"),
        "{text}"
    );

    // a repo that is not linked: a skipped-hook warning, and nothing written
    let v = setup_json(&h.w, h.project.path(), &["--code-hook", "nope"]);
    let entry = &v["hooks"][0];
    assert_eq!(entry["files"], json!([]), "{v}");
    assert_eq!(entry["warnings"][0]["code"], "W_HOOK_UNAVAILABLE", "{v}");
    assert!(
        entry["warnings"][0]["diagnostic"]["message"]
            .as_str()
            .unwrap()
            .contains("\"nope\""),
        "{v}"
    );

    // outside every project: skipped, and no file anywhere
    let w = World::new();
    let outside = short_tempdir();
    let v = setup_json(&w, outside.path(), &both);
    assert_eq!(
        v["stub"],
        json!({"result": "skipped", "reason": "outside_project"}),
        "{v}"
    );
    for entry in v["hooks"].as_array().unwrap() {
        assert_eq!(entry["files"], json!([]), "{v}");
        assert_eq!(entry["warnings"][0]["reason"], "project unavailable", "{v}");
    }
    assert!(tree(outside.path()).is_empty());
    assert!(!w.home.path().join(".claude").exists());
}

#[test]
fn m4a3_bare_setup_keeps_meta_and_stub_contract() {
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    let bare = ["files", "meta", "stub", "warnings"];
    let shown = |p: &Path| rdy_store::pio::display_path(p);

    // a repeat: written, then kept; no hooks member and no hook file
    let w = World::new();
    let pd = w.project();
    let meta = w.default_meta();
    let root = std::fs::canonicalize(pd.path()).unwrap();
    let v = setup_json(&w, pd.path(), &[]);
    assert_eq!(keys(&v), bare, "{v}");
    assert_eq!(v["meta"]["result"], "created", "{v}");
    assert_eq!(
        v["files"],
        json!([
            {"path": shown(&root.join("CLAUDE.md")), "result": "written"},
            {"path": shown(&root.join("AGENTS.md")), "result": "written"}
        ]),
        "{v}"
    );
    let out = rdy_at(&w, pd.path(), &["setup"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "Your rules project already exists at {}.\n{} already has the stub\n{} already has the stub\n",
            shown(&meta),
            shown(&root.join("CLAUDE.md")),
            shown(&root.join("AGENTS.md"))
        )
    );
    assert!(!root.join(".claude").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
        STUB
    );

    // the 0.43.0 to 0.45.0 stub is replaced in place
    std::fs::write(root.join("AGENTS.md"), format!("# Mine\n{OLD_STUB}after\n")).unwrap();
    let v = setup_json(&w, pd.path(), &[]);
    assert_eq!(keys(&v), bare, "{v}");
    assert_eq!(v["files"][1]["result"], "written", "{v}");
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        format!("# Mine\n{STUB}after\n")
    );

    // a configured meta place
    let w = World::new();
    let pd = w.project();
    let parent = short_tempdir();
    let chosen = parent.path().join("rules");
    w.person(&chosen);
    let v = setup_json(&w, pd.path(), &[]);
    assert_eq!(keys(&v), bare, "{v}");
    assert_eq!(v["meta"]["path"], chosen.to_str().unwrap(), "{v}");
    assert!(!w.default_meta().exists());

    // outside every project
    let w = World::new();
    let outside = short_tempdir();
    let v = setup_json(&w, outside.path(), &[]);
    assert_eq!(keys(&v), bare, "{v}");
    assert_eq!(
        v["stub"],
        json!({"result": "skipped", "reason": "outside_project"}),
        "{v}"
    );
    assert_eq!(v["files"], json!([]), "{v}");

    // the meta project and an alias of it
    let meta = w.default_meta();
    let v = setup_json(&w, outside.path(), &["--project", meta.to_str().unwrap()]);
    assert_eq!(keys(&v), bare, "{v}");
    assert_eq!(
        v["stub"],
        json!({"result": "skipped", "reason": "meta_project"}),
        "{v}"
    );
    let alias = w.home.path().join("meta-link");
    rdy_qa::platform::symlink(&meta, &alias).unwrap();
    if !rdy_qa::platform::follow_refused(&alias) {
        let v = setup_json(&w, &alias, &[]);
        assert_eq!(v["stub"]["reason"], "meta_project", "{v}");
        assert_eq!(keys(&v), bare, "{v}");
    }

    // a partial failure: the refusal is the one bare setup gives, and no hook runs after it
    let h = Hooked::new();
    std::fs::create_dir(h.project.path().join("AGENTS.md")).unwrap();
    for args in [
        vec!["--json", "setup"],
        vec!["--json", "setup", "--code-hook", "app"],
    ] {
        let out = rdy_at(&h.w, h.project.path(), &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let v: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(v["code"], "E_IO", "{v}");
        let agents = std::fs::canonicalize(h.project.path())
            .unwrap()
            .join("AGENTS.md");
        assert!(
            v["message"].as_str().unwrap().starts_with(&format!(
                "{} is not a regular instruction file",
                shown(&agents)
            )),
            "{v}"
        );
        assert!(!h.project.path().join("CLAUDE.md").exists());
        assert!(!h.app.join(".git").join("hooks").join("commit-msg").exists());
    }
}
