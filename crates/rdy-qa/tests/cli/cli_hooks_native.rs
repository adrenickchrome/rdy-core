//! m4a5 (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1; D-KG0V6; D-EKGN9): the two
//! installed hooks, proved as a host runs them. A real `git commit` runs the installed
//! `commit-msg` launcher, which starts the installed rdy binary; a real child process runs the
//! installed Claude Code command from its settings file, with the host event on stdin. The tests
//! are portable: the same code runs on macOS, Ubuntu and native Windows. A cross-compile does not
//! count as native coverage.
//!
//! Each fixture has a private hooks folder, because `core.hooksPath` stays unset and Git's own
//! `.git/hooks` applies (no system or global Git configuration reaches a run). Every process has
//! a home and a user config of its own ([`World`]). No test waits on a clock to prove an order:
//! a failure is injected (a binary that is gone, a project that is gone) and the result is read.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::{output_within_stdin, rdy_bin, rdy_exe_name};
use rdy_qa::instructions::World;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const NO_CITATION: &str =
    "This commit touches code anchored to a decision and has no Implements trailer.";
const DIRECT_EDIT: &str = "An agent edited rdy working-copy files directly.";

/// The Git variables a hook inherits from Git: none reaches a fixture's own Git or rdy run.
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

/// What the names of the executable, the project and the checkout hold.
#[derive(Clone, Copy)]
enum Names {
    /// Plain names.
    Plain,
    /// A space, an apostrophe and a non-ASCII letter in each.
    Awkward,
}

/// A project of a [`World`] with a landed decision and one linked code repo, `app`, whose
/// history anchors `src/anchored.rs` to the decision and leaves `src/plain.rs` unanchored. The
/// rdy binary is a copy of the built one, in a folder of its own, so a test may remove it.
struct Native {
    w: World,
    /// Keeps the folders alive.
    _tmp: tempfile::TempDir,
    /// The canonical project root.
    project: PathBuf,
    /// The canonical root of `app`.
    app: PathBuf,
    /// The copy of the rdy binary the hooks name.
    exe: PathBuf,
    global: PathBuf,
    decision: String,
}

impl Native {
    fn new(names: Names) -> Native {
        let (exe_dir, project_dir, app_dir) = match names {
            Names::Plain => ("bin", "project", "app"),
            Names::Awkward => (
                "rdy bin \u{fc} 'q'",
                "pro ject \u{fc} 'p'",
                "app \u{fc} 'co'",
            ),
        };
        rdy_qa::git_repo::require_git();
        let w = World::new();
        let tmp = short_tempdir();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        // Not under the canonical `base`: on Windows that is a `\\?\` path, which Git cannot read.
        let global = tmp.path().join("no-global");
        std::fs::write(&global, "").unwrap();
        let project = base.join(project_dir);
        w.project_at(&project);
        let project = std::fs::canonicalize(project).unwrap();
        let decision = seed_decision(&project);
        let exe_folder = base.join(exe_dir);
        std::fs::create_dir_all(&exe_folder).unwrap();
        let exe = exe_folder.join(rdy_exe_name());
        std::fs::copy(rdy_bin(), &exe).expect("copy the rdy binary");
        let app = base.join(app_dir);
        std::fs::create_dir_all(&app).unwrap();
        let n = Native {
            w,
            _tmp: tmp,
            project,
            app: std::fs::canonicalize(&app).unwrap(),
            exe,
            global,
            decision,
        };
        n.git(&n.app, &["init", "-q", "--initial-branch=main"]);
        n.write(&n.app, "src/plain.rs", "fn plain() {}\n");
        n.commit_all("feat: plain");
        n.write(&n.app, "src/anchored.rs", "fn anchored() {}\n");
        n.commit_all(&format!("feat: anchored\n\nImplements: {}", n.decision));
        rdy_qa::git_repo::link_repos(&n.project, &[("app", n.app.to_str().unwrap())]);
        n
    }

    fn git_command(&self, dir: &Path, args: &[&str]) -> Command {
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
        git_isolated(&mut c, &self.global);
        c
    }

    /// Git in `dir` (a path of this fixture), which must succeed; its trimmed stdout.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.git_command(dir, args).output().expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Stages everything in `app` and commits it with no hook.
    fn commit_all(&self, message: &str) {
        self.git(&self.app, &["add", "-A"]);
        self.git(&self.app, &["commit", "-q", "--no-verify", "-m", message]);
    }

    fn head(&self) -> String {
        self.git(&self.app, &["rev-parse", "HEAD"])
    }

    /// `rdy --project <project> <args>` of `exe` in `cwd`, through the daemon or, with `direct`,
    /// in its own process.
    fn command(&self, exe: &Path, cwd: &Path, direct: bool, args: &[&str]) -> Command {
        let mut cmd = Command::new(exe);
        cmd.arg("--project").arg(&self.project).args(args);
        cmd.current_dir(cwd);
        self.w.isolate(&mut cmd);
        git_isolated(&mut cmd, &self.global);
        cmd.env_remove("RDY_ACTOR").env_remove("RDY_RUN");
        if direct {
            cmd.env("RDY_NO_DAEMON", "1");
        } else {
            cmd.env_remove("RDY_NO_DAEMON");
        }
        cmd
    }

    /// The JSON on stdout of `rdy --json <args>`, which must exit 0.
    fn ok_json(&self, exe: &Path, direct: bool, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let mut cmd = self.command(exe, &self.project, direct, &all);
        let out = self.w.run(&mut cmd).expect("run rdy");
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

    /// Installs the code hook for `app` with this fixture's copy of rdy, in its own process.
    fn install_code(&self) -> PathBuf {
        let v = self.ok_json(
            &self.exe,
            true,
            &["hook", "install", "--kind", "code", "--repo", "app"],
        );
        assert_eq!(v["files"][0]["result"], "written", "{v}");
        let launcher = self.app.join(".git").join("hooks").join("commit-msg");
        assert!(launcher.is_file(), "{v}");
        launcher
    }

    /// Installs the Claude Code edit hook with this fixture's copy of rdy, in its own process;
    /// the settings file.
    fn install_edit(&self) -> PathBuf {
        let v = self.ok_json(
            &self.exe,
            true,
            &[
                "hook",
                "install",
                "--kind",
                "edit",
                "--harness",
                "claude-code",
            ],
        );
        assert_eq!(v["files"][0]["result"], "written", "{v}");
        let settings = self.project.join(".claude").join("settings.local.json");
        assert!(settings.is_file(), "{v}");
        settings
    }

    /// Runs `git commit` in `app` on what is staged, with the message `message`, as a person
    /// does: the installed `commit-msg` hook runs. `direct` is what the hook's rdy run sees in
    /// its environment. The commit must succeed and keep its message bytes; the output is
    /// returned.
    fn commit(&self, message: &str, direct: bool) -> Output {
        let file = self.app.join(".git").join("MSG-m4a5");
        std::fs::write(&file, message).unwrap();
        // Relative to the checkout (Git runs there): the canonical path is `\\?\` on Windows.
        let mut cmd = self.git_command(&self.app, &["commit", "-q", "-F", ".git/MSG-m4a5"]);
        self.w.isolate(&mut cmd);
        cmd.env_remove("RDY_ACTOR").env_remove("RDY_RUN");
        if direct {
            cmd.env("RDY_NO_DAEMON", "1");
        } else {
            cmd.env_remove("RDY_NO_DAEMON");
        }
        let before = self.head();
        let out = self.w.run(&mut cmd).expect("git commit");
        assert!(
            out.status.success(),
            "the hook never fails a commit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(self.head(), before, "the commit was made");
        let raw = self.git(&self.app, &["cat-file", "commit", "HEAD"]);
        let body = raw.split_once("\n\n").unwrap().1;
        assert_eq!(body.trim_end(), message.trim_end(), "the message stands");
        out
    }

    /// Changes `rel` of `app` so that `commit` has a staged change.
    fn touch(&self, rel: &str, label: &str) {
        self.write(&self.app, rel, &format!("fn x() {{ \"{label}\"; }}\n"));
        self.git(&self.app, &["add", rel]);
    }

    /// The installed Claude Code command, from the settings file: the program and its arguments.
    fn edit_command(&self, settings: &Path) -> (String, Vec<String>) {
        let v: Value = serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        let entry = &v["hooks"]["PostToolUse"][0];
        assert_eq!(entry["matcher"], "Edit|Write", "{v}");
        let c = &entry["hooks"][0];
        assert_eq!(c["type"], "command", "{v}");
        assert_eq!(c["onFailure"], "continue", "{v}");
        let args = c["args"]
            .as_array()
            .unwrap_or_else(|| panic!("exec form: {v}"))
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        (c["command"].as_str().unwrap().to_string(), args)
    }

    /// Runs the installed command as the host does: a child process of its own, the event on
    /// stdin.
    fn run_edit_command(&self, command: &str, args: &[String], stdin: &[u8]) -> Output {
        let mut cmd = Command::new(command);
        cmd.args(args).current_dir(&self.app);
        self.w.isolate(&mut cmd);
        git_isolated(&mut cmd, &self.global);
        cmd.env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env("RDY_NO_DAEMON", "1");
        output_within_stdin(&mut cmd, Some(stdin), Duration::from_secs(120)).expect("run command")
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

/// A Claude Code `PostToolUse` envelope for `tool` on `path`.
fn envelope(tool: &str, path: &Path) -> Vec<u8> {
    json!({
        "session_id": "s-1",
        "transcript_path": "/home/someone/.claude/projects/x/transcript-secret.jsonl",
        "cwd": "/somewhere",
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": {"file_path": path, "content": "body-secret-text"},
        "tool_response": {"filePath": path, "success": true}
    })
    .to_string()
    .into_bytes()
}

/// The additional context of a host reply, which carries only that and the event name.
fn context_of(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["hookSpecificOutput"], "{v}");
    let inner = &v["hookSpecificOutput"];
    assert_eq!(inner["hookEventName"], "PostToolUse", "{v}");
    assert!(
        inner.get("decision").is_none() && v.get("decision").is_none(),
        "{v}"
    );
    let text = inner["additionalContext"].as_str().unwrap().to_string();
    for secret in ["secret", "transcript"] {
        assert!(!text.contains(secret), "{text}");
    }
    text
}

/// Every file under `dir`, but the store (`.rdy`), with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.strip_prefix(base).unwrap() == Path::new(".rdy") {
                continue;
            }
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.insert(
                    p.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn m4a5_installed_git_hook_warns_and_commit_succeeds() {
    let n = Native::new(Names::Plain);
    n.install_code();
    // a daemon already runs, so the hook's deadline never covers a daemon start
    let status =
        n.w.run(&mut n.command(&n.exe, &n.project, false, &["status"]));
    assert!(status.expect("run rdy").status.success());

    // uncited, on code anchored to the decision: the warning, and the commit stands
    n.touch("src/anchored.rs", "uncited");
    let out = n.commit("feat: change anchored code\n", false);
    let err = stderr(&out);
    assert!(err.contains("W_HOOK_NO_CITATION"), "{err}");
    assert!(err.contains(NO_CITATION), "{err}");

    // cited: no warning at all
    n.touch("src/anchored.rs", "cited");
    let out = n.commit(
        &format!(
            "feat: change anchored code again\n\nImplements: {}\n",
            n.decision
        ),
        false,
    );
    let err = stderr(&out);
    assert!(!err.contains("W_HOOK"), "{err}");

    // unanchored code, uncited: no warning
    n.touch("src/plain.rs", "unanchored");
    let out = n.commit("feat: change plain code\n", false);
    let err = stderr(&out);
    assert!(!err.contains("W_HOOK"), "{err}");

    // a commit that touches both kinds of code, uncited: the warning shows
    n.touch("src/anchored.rs", "both-a");
    n.touch("src/plain.rs", "both-p");
    let out = n.commit("feat: change both\n", false);
    assert!(
        stderr(&out).contains("W_HOOK_NO_CITATION"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn m4a5_git_hook_survives_missing_binary_and_project() {
    let n = Native::new(Names::Plain);
    let launcher = n.install_code();
    let bytes = std::fs::read(&launcher).unwrap();

    // the installed binary works: the warning shows
    n.touch("src/anchored.rs", "works");
    let out = n.commit("feat: works\n", true);
    assert!(
        stderr(&out).contains("W_HOOK_NO_CITATION"),
        "{}",
        stderr(&out)
    );

    // the binary is no program: the launcher cannot fail the commit
    std::fs::write(&n.exe, b"not a program").unwrap();
    n.touch("src/anchored.rs", "garbage");
    n.commit("feat: garbage binary\n", true);

    // the binary is gone
    std::fs::remove_file(&n.exe).unwrap();
    n.touch("src/anchored.rs", "gone");
    n.commit("feat: no binary\n", true);
    assert_eq!(
        std::fs::read(&launcher).unwrap(),
        bytes,
        "the launcher stands"
    );

    // the binary is back; the project's config is gone
    std::fs::copy(rdy_bin(), &n.exe).unwrap();
    let cfg = n.project.join(".rdy.yaml");
    let aside = n.project.join(".rdy.yaml.aside");
    std::fs::rename(&cfg, &aside).unwrap();
    n.touch("src/anchored.rs", "no-config");
    let out = n.commit("feat: no config\n", true);
    std::fs::rename(&aside, &cfg).unwrap();
    let err = stderr(&out);
    assert!(err.contains("W_HOOK_UNAVAILABLE"), "{err}");
    assert!(err.contains("project unavailable"), "{err}");

    // the project's store is gone
    let store = n.project.join(".rdy");
    let aside = n.project.join(".rdy.aside");
    assert!(store.is_dir());
    std::fs::rename(&store, &aside).unwrap();
    n.touch("src/anchored.rs", "no-store");
    let out = n.commit("feat: no store\n", true);
    std::fs::rename(&aside, &store).unwrap();
    assert!(
        stderr(&out).contains("W_HOOK_UNAVAILABLE"),
        "{}",
        stderr(&out)
    );

    // everything is back: the hook warns again
    n.touch("src/anchored.rs", "back");
    let out = n.commit("feat: back\n", true);
    assert!(
        stderr(&out).contains("W_HOOK_NO_CITATION"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn m4a5_native_paths_preserve_arguments() {
    let n = Native::new(Names::Awkward);
    let launcher = n.install_code();
    let settings = n.install_edit();

    // the launcher is the seam's own text for these paths: each value quoted as data
    let want = rdy_daemon::sys::code_launcher(&n.exe, &n.project, "app")
        .expect("the seam names these paths");
    assert_eq!(std::fs::read(&launcher).unwrap(), want);
    let text = String::from_utf8(want).unwrap();
    assert!(text.contains(" --project '"), "{text}");
    assert!(text.contains("'app' --message-file \"$1\""), "{text}");
    assert!(text.contains('\u{fc}'), "{text}");

    // the settings name the program and the project as an argument vector, never a shell line
    let (command, args) = n.edit_command(&settings);
    let exec = |p: &Path| rdy_daemon::sys::hook_exec_path(p).unwrap();
    assert_eq!(command, exec(&n.exe));
    assert_eq!(
        args,
        [
            "--project".to_string(),
            exec(&n.project),
            "hook".into(),
            "run-edit".into(),
            "--harness".into(),
            "claude-code".into()
        ]
    );
    for t in [&command, &args[1]] {
        assert!(
            t.contains(' ') && t.contains('\'') && t.contains('\u{fc}'),
            "{t}"
        );
        assert!(!t.starts_with(r"\\?\"), "a plain native path: {t}");
        if cfg!(windows) {
            let b = t.as_bytes();
            assert!(
                b[0].is_ascii_alphabetic() && b[1] == b':',
                "a drive path: {t}"
            );
        }
    }

    // a real commit in the awkward checkout runs the launcher with the paths intact
    n.touch("src/anchored.rs", "awkward");
    let out = n.commit("feat: awkward paths\n", true);
    let err = stderr(&out);
    assert!(err.contains("W_HOOK_NO_CITATION"), "{err}");
    assert!(!err.contains("W_HOOK_UNAVAILABLE"), "{err}");

    // the installed edit command, started from the awkward folder with the same arguments
    let checkout = n.ok_json(&n.exe, true, &["checkout"]);
    let doc = n.project.join("spec.yaml");
    assert!(doc.is_file(), "{checkout}");
    let out = n.run_edit_command(&command, &args, &envelope("Edit", &doc));
    let ctx = context_of(&out);
    assert!(ctx.contains("W_HOOK_DIRECT_EDIT"), "{ctx}");
    assert!(ctx.contains(&rdy_store::pio::display_path(&doc)), "{ctx}");
}

#[test]
fn m4a5_edit_callback_keeps_the_edit() {
    let n = Native::new(Names::Plain);
    let settings = n.install_edit();
    let (command, args) = n.edit_command(&settings);
    let checkout = n.ok_json(&n.exe, true, &["checkout"]);
    let doc = n.project.join("spec.yaml");
    assert!(doc.is_file(), "{checkout}");
    let code = n.app.join("src/plain.rs");

    // an edit of a working-copy member: the warning is context, and nothing changes
    let before = tree(&n.project);
    let out = n.run_edit_command(&command, &args, &envelope("Write", &doc));
    let ctx = context_of(&out);
    assert!(ctx.contains("W_HOOK_DIRECT_EDIT"), "{ctx}");
    assert!(ctx.contains(DIRECT_EDIT), "{ctx}");
    assert_eq!(
        tree(&n.project),
        before,
        "the file and its folder are as they were"
    );

    // an edit that made the member's YAML invalid: it still warns, and its bytes stand
    std::fs::write(&doc, b"title: [unclosed\n  : :\n").unwrap();
    let before = tree(&n.project);
    let out = n.run_edit_command(&command, &args, &envelope("Edit", &doc));
    let ctx = context_of(&out);
    assert!(ctx.contains("W_HOOK_DIRECT_EDIT"), "{ctx}");
    assert_eq!(tree(&n.project), before);
    assert_eq!(std::fs::read(&doc).unwrap(), b"title: [unclosed\n  : :\n");

    // a normal code file of another repository: no output
    let app_before = tree(&n.app.join("src"));
    let out = n.run_edit_command(&command, &args, &envelope("Write", &code));
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(tree(&n.app.join("src")), app_before);

    // an event that is no JSON, and a project that is gone: a warning, and exit 0 still
    let out = n.run_edit_command(&command, &args, b"not json");
    let ctx = context_of(&out);
    assert!(
        ctx.contains("W_HOOK_UNAVAILABLE") && ctx.contains("invalid event"),
        "{ctx}"
    );
    let mut missing = args.clone();
    missing[1] = n
        .project
        .join("no such project")
        .to_str()
        .unwrap()
        .to_string();
    let before = tree(&n.project);
    let out = n.run_edit_command(&command, &missing, &envelope("Write", &doc));
    let ctx = context_of(&out);
    assert!(ctx.contains("W_HOOK_UNAVAILABLE"), "{ctx}");
    assert_eq!(tree(&n.project), before);
}
