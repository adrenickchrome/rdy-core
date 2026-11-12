//! m4a2: the local hook host (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ, D-DC5D1; D-KG0V6). The
//! installer writes only absent files, keeps exact owned ones, never overwrites a person's file
//! or edit, refuses links and files that change under it, and keeps what it finished. The `hook`
//! op runs under one run context from the ctl connection to the Git child: a lost connection or
//! the daemon's own deadline stops the check, and the handler returns only after the child is
//! gone. Every Git repository here has its own private hooks folder (W5).
use rdy_daemon::hooks::{EDIT_EVENT, EDIT_MATCHER, LocalHooks, SETTINGS_DIR, SETTINGS_FILE};
use rdy_daemon::{
    CtlRequest, ErrorReply, Handler, OpContext, ServeHandler, SessionInfo, Shared, dispatch,
    dispatch_with,
};
use rdy_engine::engine::Local;
use rdy_engine::hooks::{HookKind, RunContext, Stop};
use rdy_engine::signing_crypto::SigningAgent;
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_PERMISSION, HOOK_HOST_ABSENT_MESSAGE, Rdy, RdyError, Session,
    W_HOOK_CONFLICT, W_HOOK_UNAVAILABLE,
};
use rdy_model::Role;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

/// Runs git in `dir` with no system or global configuration of the machine.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join(".no-global-config"))
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project at `<tmp>/<project>` with the linked code repo `app` at `<tmp>/<checkout>`, one
/// commit in it, and the host run by `<tmp>/<exe>`. The checkout's hooks folder is its own
/// `.git/hooks`, named in its local config so no machine-wide `core.hooksPath` applies.
struct World {
    tmp: tempfile::TempDir,
    project: PathBuf,
    checkout: PathBuf,
    exe: PathBuf,
    host: Arc<LocalHooks>,
    engine: Local,
}

impl World {
    fn new() -> World {
        World::named("project", "app checkout", "bin/rdy")
    }

    /// Names with spaces, an apostrophe and Unicode, so every launcher and settings value is
    /// quoted as data.
    fn awkward() -> World {
        World::named("proj 'ü' x", "app co ü", "bin dir/it's ü/rdy")
    }

    fn named(project: &str, checkout: &str, exe: &str) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let project = base.join(project);
        let checkout = base.join(checkout);
        let exe = base.join(exe);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        let r = Rdy::init(&project, "rdy-standard").unwrap();
        let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
        yaml.push_str("code_repos: [app]\n");
        std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
        // JSON is YAML: a native Windows path stays one quoted, escaped string
        let local = json!({"code_paths": {"app": checkout.display().to_string()}});
        std::fs::write(project.join(".rdy/local.yaml"), local.to_string()).unwrap();
        git(&checkout, &["init", "-q", "--initial-branch=main"]);
        let hooks = checkout.join(".git").join("hooks");
        git(
            &checkout,
            &["config", "core.hooksPath", &hooks.display().to_string()],
        );
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::write(checkout.join("src/a.rs"), "fn a() {}\n").unwrap();
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "--no-verify", "-m", "feat: a"]);
        let host = Arc::new(LocalHooks::new(&project, &exe));
        let engine = Local::new(r).with_hook_host(host.clone());
        World {
            tmp,
            project,
            checkout,
            exe,
            host,
            engine,
        }
    }

    fn hook_file(&self) -> PathBuf {
        self.checkout.join(".git").join("hooks").join("commit-msg")
    }

    fn settings(&self) -> PathBuf {
        self.project.join(SETTINGS_DIR).join(SETTINGS_FILE)
    }

    /// An unrelated folder: the session's working directory, which the host never reads (C30).
    fn elsewhere(&self) -> PathBuf {
        let d = self.tmp.path().join("elsewhere");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run_as(&self, role: Role, args: Value) -> Result<Value, RdyError> {
        let s = Session::human("human:t".into(), role).unwrap();
        let cwd = self.elsewhere();
        let cx = OpContext {
            engine: &self.engine,
            session: &s,
            human: "human:t",
            cwd: &cwd,
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        dispatch(&cx, "hook", args)
    }

    fn run(&self, args: Value) -> Value {
        self.run_as(Role::Writer, args.clone())
            .unwrap_or_else(|e| panic!("{args}: {e}"))
    }

    fn head(&self) -> String {
        git(&self.checkout, &["rev-parse", "HEAD"])
    }

    fn code_check(&self) -> Value {
        json!({"op": "check", "kind": "code", "repo": "app",
               "checkout": self.checkout.display().to_string(), "head": self.head(),
               "paths": ["src/a.rs"], "message": "feat: b\n"})
    }
}

fn install_code() -> Value {
    json!({"op": "install", "kind": "code", "repo": "app"})
}

fn remove_code() -> Value {
    json!({"op": "remove", "kind": "code", "repo": "app"})
}

fn install_edit() -> Value {
    json!({"op": "install", "kind": "edit", "harness": "claude-code"})
}

fn remove_edit() -> Value {
    json!({"op": "remove", "kind": "edit", "harness": "claude-code"})
}

/// The one file result of an install or remove, and its warnings.
#[track_caller]
fn result(v: &Value) -> (&str, &str) {
    let files = v["files"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert_eq!(files.len(), 1, "{v}");
    (
        files[0]["path"].as_str().unwrap(),
        files[0]["result"].as_str().unwrap(),
    )
}

#[track_caller]
fn conflict(v: &Value, reason: &str) {
    assert_eq!(result(v).1, "skipped", "{v}");
    let w = &v["warnings"][0];
    assert_eq!(w["code"], W_HOOK_CONFLICT, "{v}");
    assert_eq!(w["reason"], reason, "{v}");
    let path = result(v).0;
    assert_eq!(
        w["message"],
        format!("The rdy hook left {path} unchanged: {reason}."),
        "{v}"
    );
    assert_eq!(
        w["fix"],
        "Review the existing file, then install or remove the rdy hook explicitly."
    );
}

#[track_caller]
fn clean(v: &Value, expected: &str) {
    assert_eq!(result(v).1, expected, "{v}");
    assert_eq!(v["warnings"], json!([]), "{v}");
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn json_of(p: &Path) -> Value {
    serde_json::from_slice(&read(p)).unwrap()
}

/// The names in `dir`, so a test sees that no temporary file was left.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Makes `link` a symbolic link to `target`; false (the case is skipped) when this Windows
/// account lacks the symbolic-link privilege.
fn link(target: &Path, link: &Path) -> bool {
    match rdy_store::pio::symlink(target, link) {
        Ok(()) => true,
        Err(e) if cfg!(windows) => {
            eprintln!("skip: this account cannot make symbolic links ({e})");
            false
        }
        Err(e) => panic!("{}: {e}", link.display()),
    }
}

/// Registers the worktree `name` of `project` at `dir`, as `rdy worktree` records one.
fn register_worktree(project: &Path, name: &str, dir: &Path) {
    let state = project.join(".rdy/worktrees").join(name);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("dir.txt"), dir.display().to_string()).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let ptr = json!({"project": project.display().to_string(), "name": name});
    std::fs::write(dir.join(".rdy-worktree"), ptr.to_string()).unwrap();
}

#[test]
fn m4a2_install_is_opt_in_idempotent_and_reversible() {
    let w = World::awkward();
    // opt in: nothing exists until an install names it
    assert!(!w.hook_file().exists());
    assert!(!w.project.join(SETTINGS_DIR).exists());

    // the code hook: the exact launcher, executable, then kept
    let v = w.run(install_code());
    clean(&v, "written");
    assert!(result(&v).0.ends_with("commit-msg"), "{v}");
    assert_eq!(v["kind"], "code");
    let launcher = w.host.code_launcher("app").unwrap();
    assert_eq!(read(&w.hook_file()), launcher);
    let text = String::from_utf8(launcher.clone()).unwrap();
    assert!(text.starts_with("#!/bin/sh\n"), "{text}");
    assert!(text.ends_with("exit 0\n"), "{text}");
    assert!(text.contains(" hook run-code --repo 'app' --message-file \"$1\"\n"));
    let meta = std::fs::metadata(w.hook_file()).unwrap();
    assert!(rdy_daemon::sys::launcher_is_executable(&meta));
    clean(&w.run(install_code()), "kept");
    assert_eq!(read(&w.hook_file()), launcher);

    // the edit hook: the exec form with the project named, then kept
    let v = w.run(install_edit());
    clean(&v, "written");
    assert_eq!(v["kind"], "edit");
    assert_eq!(read(&w.settings()), w.host.fresh_settings().unwrap());
    let settings = json_of(&w.settings());
    let entry = &settings["hooks"][EDIT_EVENT][0];
    assert_eq!(entry["matcher"], EDIT_MATCHER);
    let command = &entry["hooks"][0];
    assert_eq!(command["type"], "command");
    assert_eq!(command["onFailure"], "continue");
    assert_eq!(
        command["command"],
        rdy_daemon::sys::hook_exec_path(&w.exe).unwrap()
    );
    assert_eq!(
        command["args"],
        json!([
            "--project",
            rdy_daemon::sys::hook_exec_path(&w.project).unwrap(),
            "hook",
            "run-edit",
            "--harness",
            "claude-code"
        ])
    );
    clean(&w.run(install_edit()), "kept");

    // a registered worktree gets its own settings; the main root is untouched by it
    let wt = w.tmp.path().join("wt ü");
    register_worktree(&w.project, "feature", &wt);
    let before = read(&w.settings());
    let v = w.run(
        json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "feature"}),
    );
    clean(&v, "written");
    let wt_settings = std::fs::canonicalize(&wt)
        .unwrap()
        .join(SETTINGS_DIR)
        .join(SETTINGS_FILE);
    assert_eq!(read(&wt_settings), w.host.fresh_settings().unwrap());
    assert_eq!(read(&w.settings()), before);
    clean(
        &w.run(json!({"op": "remove", "kind": "edit", "harness": "claude-code", "worktree": "feature"})),
        "removed",
    );
    assert!(!wt_settings.exists());

    // explicit targets only: an unknown repo or worktree is a refused form, with a hint
    let e = w
        .run_as(
            Role::Writer,
            json!({"op": "install", "kind": "code", "repo": "ap"}),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("(did you mean \"app\"?)"), "{e}");
    let e = w
        .run_as(
            Role::Writer,
            json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "featur"}),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("(did you mean \"feature\"?)"), "{e}");

    // reversible: remove takes exactly what install wrote, and a second remove has nothing
    clean(&w.run(remove_code()), "removed");
    assert!(!w.hook_file().exists());
    clean(&w.run(remove_code()), "kept");
    clean(&w.run(remove_edit()), "removed");
    assert!(!w.settings().exists());
    clean(&w.run(remove_edit()), "kept");
    // nothing was written where the session stood
    assert_eq!(names(&w.elsewhere()), Vec::<String>::new());
    assert!(!w.elsewhere().join(SETTINGS_DIR).exists());
}

/// The launcher, run as Git runs it, passes every value as one argument and exits 0, even with
/// no binary. Windows runs it through Git for Windows' own shell, which the native gate covers.
#[test]
fn m4a2_launcher_quotes_values_and_always_exits_zero() {
    let w = World::awkward();
    if cfg!(windows) {
        eprintln!("skip: the launcher runs under Git for Windows' shell in the native gate");
        return;
    }
    let launcher = w.tmp.path().join("commit-msg");
    std::fs::write(&launcher, w.host.code_launcher("app").unwrap()).unwrap();
    let message = w.tmp.path().join("MSG ü 'x'");
    std::fs::write(&message, "feat: x\n").unwrap();
    let run = || {
        Command::new("sh")
            .arg(&launcher)
            .arg(&message)
            .status()
            .unwrap()
    };
    // no binary at all: the commit goes ahead
    assert!(run().success());
    // a stand-in binary that records its arguments, one per line
    let bin = w.exe.parent().unwrap();
    std::fs::create_dir_all(bin).unwrap();
    std::fs::write(
        &w.exe,
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$(dirname \"$0\")/args\"\nexit 3\n",
    )
    .unwrap();
    let dir = rdy_store::pio::open_dir(bin).unwrap();
    rdy_daemon::sys::make_launcher_executable(&dir, std::ffi::OsStr::new("rdy")).unwrap();
    assert!(run().success(), "a failing rdy still exits 0");
    let args = std::fs::read_to_string(bin.join("args")).unwrap();
    let project = std::fs::canonicalize(&w.project).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "--project",
            project.to_str().unwrap(),
            "hook",
            "run-code",
            "--repo",
            "app",
            "--message-file",
            message.to_str().unwrap(),
        ]
    );
}

#[test]
fn m4a2_existing_hooks_and_settings_survive() {
    let w = World::new();
    // a person's own commit-msg stays, on install and on remove
    let mine = b"#!/bin/sh\necho mine\nexit 1\n";
    std::fs::write(w.hook_file(), mine).unwrap();
    conflict(&w.run(install_code()), "existing hook");
    conflict(&w.run(remove_code()), "existing hook");
    assert_eq!(read(&w.hook_file()), mine);
    // a file larger than any launcher is refused unread
    let big = vec![b'#'; rdy_engine::hooks::MAX_LAUNCHER_BYTES + 1];
    std::fs::write(w.hook_file(), &big).unwrap();
    conflict(&w.run(install_code()), "existing hook");
    assert_eq!(read(&w.hook_file()), big);
    std::fs::remove_file(w.hook_file()).unwrap();

    // an edited owned launcher is the person's now
    clean(&w.run(install_code()), "written");
    let mut edited = read(&w.hook_file());
    edited.extend_from_slice(b"echo extra\n");
    std::fs::write(w.hook_file(), &edited).unwrap();
    conflict(&w.run(install_code()), "changed owned file");
    conflict(&w.run(remove_code()), "changed owned file");
    assert_eq!(read(&w.hook_file()), edited);
    std::fs::remove_file(w.hook_file()).unwrap();

    // a hooks path outside the checkout's Git folder is never written
    let outside = w.tmp.path().join("shared-hooks");
    std::fs::create_dir_all(&outside).unwrap();
    git(
        &w.checkout,
        &["config", "core.hooksPath", &outside.display().to_string()],
    );
    conflict(&w.run(install_code()), "external hooks path");
    assert_eq!(names(&outside), Vec::<String>::new());
    git(
        &w.checkout,
        &[
            "config",
            "core.hooksPath",
            &w.checkout.join(".git/hooks").display().to_string(),
        ],
    );

    // unrelated members and hook entries survive an install and a remove
    std::fs::create_dir_all(w.project.join(SETTINGS_DIR)).unwrap();
    let theirs = json!({
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard"}]}],
            "PostToolUse": [{"matcher": "Write", "hooks": [{"type": "command", "command": "fmt"}]}]
        },
        "z": 1
    });
    std::fs::write(w.settings(), serde_json::to_vec(&theirs).unwrap()).unwrap();
    clean(&w.run(install_edit()), "written");
    let merged = json_of(&w.settings());
    assert_eq!(merged["permissions"], theirs["permissions"]);
    assert_eq!(merged["z"], 1);
    assert_eq!(merged["hooks"]["PreToolUse"], theirs["hooks"]["PreToolUse"]);
    let post = merged["hooks"][EDIT_EVENT].as_array().unwrap();
    assert_eq!(post.len(), 2);
    assert_eq!(post[0], theirs["hooks"]["PostToolUse"][0]);
    assert_eq!(post[1], w.host.edit_entry().unwrap());
    clean(&w.run(install_edit()), "kept");
    clean(&w.run(remove_edit()), "removed");
    assert_eq!(json_of(&w.settings()), theirs);
    clean(&w.run(remove_edit()), "kept");

    // an edited owned entry is the person's now
    clean(&w.run(install_edit()), "written");
    let mut s = json_of(&w.settings());
    s["hooks"][EDIT_EVENT][1]["matcher"] = json!("Edit");
    let edited = serde_json::to_vec(&s).unwrap();
    std::fs::write(w.settings(), &edited).unwrap();
    conflict(&w.run(install_edit()), "changed owned file");
    conflict(&w.run(remove_edit()), "changed owned file");
    assert_eq!(read(&w.settings()), edited);

    // settings that cannot be merged are never written
    for bad in [
        &b"{\"hooks\": "[..],
        &b"[1, 2]"[..],
        &b"{\"hooks\": []}"[..],
        &b"{\"hooks\": {\"PostToolUse\": {}}}"[..],
    ] {
        std::fs::write(w.settings(), bad).unwrap();
        conflict(&w.run(install_edit()), "invalid settings");
        conflict(&w.run(remove_edit()), "invalid settings");
        assert_eq!(read(&w.settings()), bad);
    }
    let huge = vec![b' '; rdy_engine::hooks::MAX_HOOK_INPUT + 1];
    std::fs::write(w.settings(), &huge).unwrap();
    conflict(&w.run(install_edit()), "invalid settings");
    assert_eq!(read(&w.settings()).len(), huge.len());
    assert_eq!(names(&w.project.join(SETTINGS_DIR)), [SETTINGS_FILE]);
}

#[test]
fn m4a2_installer_refuses_links_and_concurrent_replacement() {
    let w = World::new();
    let outside = w.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("target");
    std::fs::write(&target, b"theirs").unwrap();

    // a link at the hook or the settings file is never followed
    if link(&target, &w.hook_file()) {
        conflict(&w.run(install_code()), "unsafe path");
        conflict(&w.run(remove_code()), "unsafe path");
        std::fs::remove_file(w.hook_file()).unwrap();
    }
    std::fs::create_dir_all(w.project.join(SETTINGS_DIR)).unwrap();
    if link(&target, &w.settings()) {
        conflict(&w.run(install_edit()), "unsafe path");
        conflict(&w.run(remove_edit()), "unsafe path");
        std::fs::remove_file(w.settings()).unwrap();
    }
    std::fs::remove_dir(w.project.join(SETTINGS_DIR)).unwrap();
    // nor a link (a reparse point on Windows) at the settings or the hooks folder
    if link(&outside, &w.project.join(SETTINGS_DIR)) {
        conflict(&w.run(install_edit()), "unsafe path");
        rdy_store::pio::remove_file_at(
            &rdy_store::pio::open_dir(&w.project).unwrap(),
            std::ffi::OsStr::new(SETTINGS_DIR),
        )
        .or_else(|_| std::fs::remove_dir(w.project.join(SETTINGS_DIR)))
        .unwrap();
    }
    let hooks = w.checkout.join(".git/hooks");
    std::fs::rename(&hooks, w.tmp.path().join("hooks-aside")).unwrap();
    if link(&outside, &hooks) {
        conflict(&w.run(install_code()), "unsafe path");
        rdy_store::pio::remove_file_at(
            &rdy_store::pio::open_dir(&w.checkout.join(".git")).unwrap(),
            std::ffi::OsStr::new("hooks"),
        )
        .or_else(|_| std::fs::remove_dir(&hooks))
        .unwrap();
    }
    std::fs::rename(w.tmp.path().join("hooks-aside"), &hooks).unwrap();
    assert_eq!(read(&target), b"theirs");
    assert_eq!(names(&outside), ["target"]);

    // a busy installer changes nothing: the lock is held and the deadline has passed
    let held = w.host.hold_lock_for_tests().unwrap();
    let past = RunContext::new().with_deadline(Instant::now());
    let busy = rdy_engine::hooks::HookHost::install(
        &*w.host,
        &rdy_engine::hooks::HookTarget::Code { repo: "app".into() },
        &past,
    )
    .unwrap();
    conflict(&serde_json::to_value(&busy).unwrap(), "installer busy");
    assert!(!w.hook_file().exists());
    let s = Session::human("human:t".into(), Role::Writer).unwrap();
    let cwd = w.elsewhere();
    let cx = OpContext {
        engine: &w.engine,
        session: &s,
        human: "human:t",
        cwd: &cwd,
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let busy = dispatch_with(&cx, "hook", install_edit(), &past).unwrap();
    conflict(&busy, "installer busy");
    assert!(!w.project.join(SETTINGS_DIR).exists());
    drop(held);

    // settings that change before the replacement is published stay the person's
    std::fs::create_dir_all(w.project.join(SETTINGS_DIR)).unwrap();
    std::fs::write(w.settings(), b"{\"a\": 1}").unwrap();
    w.host.before_publish_for_tests(|p| {
        if p.ends_with(SETTINGS_FILE) {
            std::fs::write(p, b"{\"a\": 2}").unwrap();
        }
    });
    conflict(&w.run(install_edit()), "file changed");
    assert_eq!(read(&w.settings()), b"{\"a\": 2}");
    assert_eq!(names(&w.project.join(SETTINGS_DIR)), [SETTINGS_FILE]);
    // a file that appears before an absent one is created stays the person's
    std::fs::remove_file(w.settings()).unwrap();
    w.host.before_publish_for_tests(|p| {
        if p.ends_with(SETTINGS_FILE) {
            std::fs::write(p, b"{\"b\": 1}").unwrap();
        }
    });
    conflict(&w.run(install_edit()), "file changed");
    assert_eq!(read(&w.settings()), b"{\"b\": 1}");
    // a launcher replaced before its removal stays
    w.host.before_publish_for_tests(|_| {});
    clean(&w.run(install_code()), "written");
    w.host.before_publish_for_tests(|p| {
        if p.ends_with("commit-msg") {
            std::fs::write(p, b"#!/bin/sh\nexit 0\n").unwrap();
        }
    });
    conflict(&w.run(remove_code()), "file changed");
    assert_eq!(read(&w.hook_file()), b"#!/bin/sh\nexit 0\n");
}

#[test]
fn m4a2_install_keeps_completed_steps() {
    let w = World::new();
    // step one is durable
    clean(&w.run(install_code()), "written");
    let launcher = read(&w.hook_file());
    // step two fails after its bytes are durable: it reports a skipped file and why, and
    // leaves neither a partial file nor a temporary one
    w.host.fail_next_publish_for_tests();
    let v = w.run(install_edit());
    assert_eq!(result(&v).1, "skipped", "{v}");
    assert_eq!(v["warnings"][0]["code"], W_HOOK_UNAVAILABLE, "{v}");
    assert_eq!(v["warnings"][0]["reason"], "I/O failure", "{v}");
    assert_eq!(
        v["warnings"][0]["message"],
        "The rdy hook could not check this action: I/O failure."
    );
    assert!(!w.settings().exists());
    assert_eq!(names(&w.project.join(SETTINGS_DIR)), Vec::<String>::new());
    // the finished step stands, and a retry finishes the rest
    assert_eq!(read(&w.hook_file()), launcher);
    clean(&w.run(install_code()), "kept");
    clean(&w.run(install_edit()), "written");

    // a merge that fails leaves the settings as they were, and no temporary file
    clean(&w.run(remove_edit()), "removed");
    std::fs::write(w.settings(), b"{\"a\": 1}").unwrap();
    w.host.fail_next_publish_for_tests();
    let v = w.run(install_edit());
    assert_eq!(result(&v).1, "skipped", "{v}");
    assert_eq!(v["warnings"][0]["reason"], "I/O failure", "{v}");
    assert_eq!(read(&w.settings()), b"{\"a\": 1}");
    assert_eq!(names(&w.project.join(SETTINGS_DIR)), [SETTINGS_FILE]);
    clean(&w.run(install_edit()), "written");
    // a remove that fails keeps the file; the retry removes it
    w.host.fail_next_publish_for_tests();
    let v = w.run(remove_code());
    assert_eq!(result(&v).1, "skipped", "{v}");
    assert_eq!(read(&w.hook_file()), launcher);
    clean(&w.run(remove_code()), "removed");
}

// ---------------------------------------------------------------------------------------------
// Host runs: bounded and warn-only
// ---------------------------------------------------------------------------------------------

/// How a held run waits once its first Git child has started.
#[derive(Clone, Copy)]
enum Hold {
    /// Until the run is cancelled (its connection was lost), or else stopped by its deadline.
    UntilCancelled,
    /// Until the run's own deadline has passed.
    UntilDeadline,
}

/// What a held run ended with.
struct Ended {
    value: Result<Value, ErrorReply>,
    cancelled: bool,
    stopped: Option<Stop>,
    spawned: usize,
}

/// Waits until `done()`, on a signal of the run itself: never an elapsed-time assertion.
fn wait_until(done: impl Fn() -> bool) {
    let guard = Instant::now() + Duration::from_secs(120);
    while !done() {
        assert!(Instant::now() < guard, "the run never stopped");
        std::thread::park_timeout(Duration::from_millis(2));
    }
}

/// A run context that reports its first Git child on `spawned` and holds it there as `hold`
/// says; the child is killed and reaped by the Git executor once the run has stopped.
fn holding(
    run: &RunContext,
    hold: Hold,
    spawned: impl Fn() + Send + Sync + 'static,
    count: Arc<AtomicUsize>,
) -> RunContext {
    let watch = run.clone();
    run.clone().on_spawn(move || {
        if count.fetch_add(1, Ordering::SeqCst) > 0 {
            return;
        }
        spawned();
        match hold {
            // the deadline ends a wait whose cancellation never comes, so that is seen too
            Hold::UntilCancelled => wait_until(|| watch.stopped().is_some()),
            Hold::UntilDeadline => wait_until(|| watch.stopped() == Some(Stop::Deadline)),
        }
    })
}

/// The daemon's own handler, with a hold on its next run and a report of how each run ended.
struct Held {
    inner: ServeHandler,
    hold: Mutex<Option<Hold>>,
    spawned: tokio::sync::mpsc::UnboundedSender<()>,
    ended: tokio::sync::mpsc::UnboundedSender<Ended>,
}

impl Handler for Held {
    fn ctl(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        self.inner.ctl(s, r, agent)
    }

    fn ctl_run(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        agent: Arc<dyn SigningAgent>,
        run: &RunContext,
    ) -> Result<Value, ErrorReply> {
        let count = Arc::new(AtomicUsize::new(0));
        let hold = self.hold.lock().unwrap().take();
        let ctx = match hold {
            Some(hold) => {
                let tx = self.spawned.clone();
                holding(
                    run,
                    hold,
                    move || {
                        let _ = tx.send(());
                    },
                    count.clone(),
                )
            }
            None => run.clone(),
        };
        let value = self.inner.ctl_run(s, r, agent, &ctx);
        let _ = self.ended.send(Ended {
            value: value.clone(),
            cancelled: run.is_cancelled(),
            stopped: run.stopped(),
            spawned: count.load(Ordering::SeqCst),
        });
        value
    }

    fn session_ended(&self, s: &SessionInfo) {
        self.inner.session_ended(s);
    }
}

#[track_caller]
fn unavailable(v: &Value, reason: &str) {
    assert_eq!(v["checked"], false, "{v}");
    let w = &v["warnings"][0];
    assert_eq!(w["code"], W_HOOK_UNAVAILABLE, "{v}");
    assert_eq!(w["reason"], reason, "{v}");
    assert_eq!(
        w["message"],
        format!("The rdy hook could not check this action: {reason}.")
    );
    assert_eq!(
        w["fix"],
        "Continue your work. Run the hook check again when rdy and the project are available."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m4a2_host_runs_are_bounded_and_warn_only() {
    // the project at a short path: on Unix its socket must fit sun_path
    let dir = common::short_tempdir();
    let project = std::fs::canonicalize(dir.path()).unwrap();
    let checkout = project.join("co");
    std::fs::create_dir_all(&checkout).unwrap();
    let r = Rdy::init(&project, "rdy-standard").unwrap();
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str("code_repos: [app]\n");
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    let local = json!({"code_paths": {"app": checkout.display().to_string()}});
    std::fs::write(project.join(".rdy/local.yaml"), local.to_string()).unwrap();
    git(&checkout, &["init", "-q", "--initial-branch=main"]);
    std::fs::write(checkout.join("a.rs"), "a\n").unwrap();
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "--no-verify", "-m", "feat: a"]);
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    let check = |checkout: &Path, head: &str| {
        json!({"op": "check", "kind": "code", "repo": "app",
               "checkout": checkout.display().to_string(), "head": head,
               "paths": ["a.rs"], "message": "feat: b\n"})
    };

    let (spawned_tx, mut spawned) = tokio::sync::mpsc::unbounded_channel();
    let (ended_tx, mut ended) = tokio::sync::mpsc::unbounded_channel();
    let handler = Arc::new(Held {
        inner: ServeHandler::new(Shared::new(r), Some("human:t".into())),
        hold: Mutex::new(None),
        spawned: spawned_tx,
        ended: ended_tx,
    });
    let run = rdy_daemon::RunDir::of_project(&project);
    let mut d = rdy_daemon::Daemon::new(
        rdy_daemon::DaemonConfig::new(run.clone(), "1.0.0".to_string()),
        handler.clone(),
    );
    d.bind().await.expect("bind");
    let (handle, readiness) = (d.handle(), d.readiness());
    readiness.ready();
    let task = tokio::spawn(d.run());
    let connect = || async {
        let (mut c, first) =
            common::connect_with(&run, &common::hello("1.0.0", "ctl", &project)).await;
        assert!(first.contains("ready"), "{first}");
        let v = common::ctl(&mut c, 1, "session", json!({})).await;
        assert_eq!(v["ok"]["kind"], "human", "{v}");
        c
    };
    let mut c = connect().await;

    // a clean run, then child failures: each a warning in an ordinary reply, never an error
    let v = common::ctl(&mut c, 2, "hook", check(&checkout, &head)).await;
    assert_eq!(
        v["ok"],
        json!({"kind": "code", "checked": true, "warnings": []}),
        "{v}"
    );
    let plain = project.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let v = common::ctl(&mut c, 3, "hook", check(&plain, &head)).await;
    unavailable(&v["ok"], "checkout unavailable");
    let v = common::ctl(&mut c, 4, "hook", check(&checkout, &"0".repeat(40))).await;
    unavailable(&v["ok"], "history unavailable");
    // invalid events are refused forms; a host turns each into a warning
    let v = common::ctl(
        &mut c,
        5,
        "hook",
        json!({"op": "check", "kind": "edit", "paths": ["rel"]}),
    )
    .await;
    assert_eq!(v["error"]["code"], E_BAD_INPUT, "{v}");
    let many: Vec<String> = (0..257).map(|i| format!("/p{i}")).collect();
    let v = common::ctl(
        &mut c,
        6,
        "hook",
        json!({"op": "check", "kind": "edit", "paths": many}),
    )
    .await;
    assert_eq!(v["error"]["code"], E_LIMIT, "{v}");
    while ended.try_recv().is_ok() {}

    // the daemon's own deadline stops a held Git child while the client waits
    *handler.hold.lock().unwrap() = Some(Hold::UntilDeadline);
    let v = common::ctl(&mut c, 7, "hook", check(&checkout, &head)).await;
    unavailable(&v["ok"], "deadline");
    let e = ended.recv().await.unwrap();
    assert_eq!(e.spawned, 1, "the run held its first Git child");
    assert_eq!(e.stopped, Some(Stop::Deadline));
    assert!(!e.cancelled, "the deadline, not a lost client, stopped it");
    spawned.recv().await.unwrap();

    // a lost connection cancels a held Git child: the handler finishes after it
    let mut lost = connect().await;
    *handler.hold.lock().unwrap() = Some(Hold::UntilCancelled);
    let line = json!({"id": 9, "op": "hook", "args": check(&checkout, &head)}).to_string() + "\n";
    use tokio::io::AsyncWriteExt;
    lost.get_mut().write_all(line.as_bytes()).await.unwrap();
    spawned.recv().await.unwrap();
    drop(lost);
    let e = ended.recv().await.unwrap();
    assert!(e.cancelled, "the lost connection cancelled the run");
    assert_eq!(e.stopped, Some(Stop::Cancelled));
    assert_eq!(e.spawned, 1, "no Git child started after the cancellation");
    unavailable(e.value.as_ref().unwrap(), "deadline");
    assert_eq!(
        e.value.as_ref().unwrap()["warnings"][0]["diagnostic"]["message"],
        "git stopped: the hook run was cancelled"
    );

    // the first connection still serves
    let v = common::ctl(&mut c, 10, "hook", check(&checkout, &head)).await;
    assert_eq!(v["ok"]["checked"], true, "{v}");
    drop(c);
    handle.stop();
    task.await.unwrap().unwrap();
    drop(handler);

    // direct mode: the same cleanup with no daemon
    let w = World::new();
    let s = Session::human("human:t".into(), Role::Writer).unwrap();
    let cwd = w.elsewhere();
    let count = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let base = RunContext::new().with_deadline(Instant::now() + Duration::from_secs(600));
    let ctx = holding(
        &base,
        Hold::UntilCancelled,
        move || tx.lock().unwrap().send(()).unwrap(),
        count.clone(),
    );
    let args = w.code_check();
    let v = std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            let cx = OpContext {
                engine: &w.engine,
                session: &s,
                human: "human:t",
                cwd: &cwd,
                agent: &rdy_engine::signing_crypto::UnavailableAgent,
            };
            dispatch_with(&cx, "hook", args, &ctx)
        });
        rx.recv().unwrap();
        base.cancel();
        running.join().unwrap().unwrap()
    });
    unavailable(&v, "deadline");
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // a host's result: a refusal becomes the unavailable warning for its reason
    let refused = w.run_as(
        Role::Writer,
        json!({"op": "check", "kind": "edit", "paths": ["rel"]}),
    );
    unavailable(
        &rdy_daemon::hooks::warn_only(HookKind::Edit, refused),
        "invalid event",
    );
    let many: Vec<String> = (0..257).map(|i| format!("/p{i}")).collect();
    let over = w.run_as(
        Role::Writer,
        json!({"op": "check", "kind": "edit", "paths": many}),
    );
    unavailable(
        &rdy_daemon::hooks::warn_only(HookKind::Edit, over),
        "input limit",
    );
    let fine = w.run_as(Role::Writer, w.code_check());
    assert_eq!(
        rdy_daemon::hooks::warn_only(HookKind::Code, fine)["checked"],
        true
    );
    // an unavailable project: its configuration is gone
    std::fs::remove_file(w.project.join(".rdy.yaml")).unwrap();
    unavailable(&w.run(w.code_check()), "project unavailable");
}

/// Runs the real `dispatch_with` for a ctl connection; a request's `as` key (taken out first)
/// picks the session's role, as a session's role does in the server.
struct Roles {
    engine: Local,
    dir: PathBuf,
}

impl Roles {
    fn serve(&self, mut r: CtlRequest, run: &RunContext) -> Result<Value, ErrorReply> {
        let role = match r.args.as_object_mut().and_then(|o| o.remove("as")) {
            Some(Value::String(x)) if x == "reader" => Role::Reader,
            _ => Role::Writer,
        };
        let session = Session::human("human:t".into(), role).unwrap();
        let cx = OpContext {
            engine: &self.engine,
            session: &session,
            human: "human:t",
            cwd: &self.dir,
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        dispatch_with(&cx, &r.op, r.args, run).map_err(ErrorReply::from)
    }
}

impl Handler for Roles {
    fn ctl(
        &self,
        _s: &SessionInfo,
        r: CtlRequest,
        _agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        self.serve(r, &RunContext::new())
    }

    fn ctl_run(
        &self,
        _s: &SessionInfo,
        r: CtlRequest,
        _agent: Arc<dyn SigningAgent>,
        run: &RunContext,
    ) -> Result<Value, ErrorReply> {
        self.serve(r, run)
    }
}

#[tokio::test]
async fn m4a2_reader_cannot_install_or_remove() {
    let w = World::new();
    let dir = common::short_tempdir();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    drop(r);
    // the daemon's endpoint belongs to the short folder; the engine is the world's project
    let handler = Arc::new(Roles {
        engine: w.engine,
        dir: w.tmp.path().to_path_buf(),
    });
    let run = rdy_daemon::RunDir::of_project(dir.path());
    let mut d = rdy_daemon::Daemon::new(
        rdy_daemon::DaemonConfig::new(run.clone(), "1.0.0".to_string()),
        handler,
    );
    d.bind().await.expect("bind");
    let (handle, readiness) = (d.handle(), d.readiness());
    readiness.ready();
    let task = tokio::spawn(d.run());
    let (mut c, first) =
        common::connect_with(&run, &common::hello("1.0.0", "ctl", dir.path())).await;
    assert!(first.contains("ready"), "{first}");

    let hook_file = w.checkout.join(".git/hooks/commit-msg");
    let edited = w.project.join("guide.yaml").display().to_string();
    // a reader checks
    let v = common::ctl(
        &mut c,
        1,
        "hook",
        json!({"op": "check", "kind": "edit", "paths": [edited], "as": "reader"}),
    )
    .await;
    assert_eq!(v["ok"]["checked"], true, "{v}");
    // a reader neither installs nor removes, and nothing changes
    for (id, args, act) in [
        (2, install_code(), "install"),
        (3, remove_code(), "remove"),
        (4, install_edit(), "install"),
        (5, remove_edit(), "remove"),
    ] {
        let mut args = args;
        args["as"] = json!("reader");
        let v = common::ctl(&mut c, id, "hook", args).await;
        assert_eq!(v["error"]["code"], E_PERMISSION, "{v}");
        assert_eq!(
            v["error"]["message"],
            format!("role Reader cannot {act} hooks")
        );
        assert_eq!(v["error"]["fix"], "start the session as a writer");
    }
    assert!(!hook_file.exists());
    assert!(!w.project.join(SETTINGS_DIR).exists());
    // the person's ordinary session installs and removes
    let v = common::ctl(&mut c, 6, "hook", install_code()).await;
    assert_eq!(v["ok"]["files"][0]["result"], "written", "{v}");
    let v = common::ctl(&mut c, 7, "hook", remove_code()).await;
    assert_eq!(v["ok"]["files"][0]["result"], "removed", "{v}");
    drop(c);
    handle.stop();
    task.await.unwrap().unwrap();

    // an engine with no local host refuses installs with the exact text, and still checks
    let bare = tempfile::tempdir().unwrap();
    let engine = Local::new(Rdy::init(bare.path(), "rdy-standard").unwrap());
    let s = Session::human("human:t".into(), Role::Writer).unwrap();
    let cx = OpContext {
        engine: &engine,
        session: &s,
        human: "human:t",
        cwd: bare.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let e = dispatch(&cx, "hook", install_edit()).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, HOOK_HOST_ABSENT_MESSAGE);
    assert_eq!(
        e.fix,
        "Use rdy setup on the machine that holds the checkout."
    );
    let p = bare.path().join("x.rs").display().to_string();
    let v = dispatch(
        &cx,
        "hook",
        json!({"op": "check", "kind": "edit", "paths": [p]}),
    )
    .unwrap();
    assert_eq!(v["checked"], true, "{v}");
}
