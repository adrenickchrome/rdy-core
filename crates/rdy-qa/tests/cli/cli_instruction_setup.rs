//! ain5 (D-8X04F) and agi4 (D-PHH2Q): `rdy setup` creates the person's empty rules project (the
//! meta project) at the configured place when nothing stands there, then writes the three-line
//! stub into `CLAUDE.md` and `AGENTS.md` of the chosen project root. The stub goes first, before
//! the person's own text, which stays as it was. A repeat changes nothing. Outside a project, and
//! in the meta project (under any spelling), setup writes no stub. A failed meta step is a
//! warning and never stops the project's stubs. A link or any other file that is not a regular
//! file is refused before either file is written, and its target is never touched. `rdy init`
//! and `rdy status` prompt for setup while something is missing.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within;
use rdy_qa::instructions::World;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

const NOT_REGULAR_FIX: &str = "replace it with a regular file, then run rdy setup again";

const INIT_PROMPT: &str =
    "Run rdy setup to prepare your rules project and this project's agent instruction files.";

const STATUS_PROMPT: &str = "Your own rules project is missing. Run rdy setup.";

const NO_HOME: &str = "No home directory is available; set instructions.person to an absolute project path in user config.";

const OCCUPIED_FIX: &str = "Choose an unused absolute path with instructions.person, or move this path aside, then run rdy setup.";

/// `rdy <args>` in `cwd`, with no `--project` unless `args` names one, as the world's human.
fn rdy_at(w: &World, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
    for name in w.removed_env() {
        cmd.env_remove(name);
    }
    output_within(&mut cmd, Duration::from_secs(120)).expect("rdy ran")
}

fn json_of(out: &Output) -> Value {
    let raw = if out.status.success() {
        &out.stdout
    } else {
        &out.stderr
    };
    let text = String::from_utf8_lossy(raw);
    serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or_else(|_| panic!("JSON in {text}"))
}

/// `rdy --json <args>` in `cwd`: the reply of a command that must succeed.
fn ok_json(w: &World, cwd: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = rdy_at(w, cwd, &all);
    assert!(
        out.status.success(),
        "rdy {all:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_of(&out)
}

/// The stdout of a command that must succeed.
fn ok_text(w: &World, cwd: &Path, args: &[&str]) -> String {
    let out = rdy_at(w, cwd, args);
    assert!(
        out.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every file under `dir` with its bytes, by path.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
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

/// Whether `dir` is a project: its config and its store entry.
fn is_project(dir: &Path) -> bool {
    dir.join(".rdy.yaml").is_file() && dir.join(".rdy").join("store").exists()
}

fn no_stub_files(dir: &Path) {
    assert!(
        !dir.join("CLAUDE.md").exists() && !dir.join("AGENTS.md").exists(),
        "{} holds an instruction file",
        dir.display()
    );
}

fn created_message(meta: &Path) -> String {
    format!("Created your rules project at {}.", meta.display())
}

fn kept_message(meta: &Path) -> String {
    format!("Your rules project already exists at {}.", meta.display())
}

#[test]
fn ain5_setup_writes_exactly_three_lines_and_keeps_user_text() {
    let w = World::new();
    let pd = w.project();
    let mine = "# My agents\n\nUse tabs.\nNo trailing newline here";
    std::fs::write(pd.path().join("AGENTS.md"), mine).unwrap();
    // from a nested directory of the project: the project root takes the stub
    let nested = pd.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    let out = rdy_at(&w, &nested, &["setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let claude = read(&pd.path().join("CLAUDE.md"));
    assert_eq!(claude, STUB);
    assert_eq!(claude.lines().count(), 3);
    let agents = read(&pd.path().join("AGENTS.md"));
    assert_eq!(agents, format!("{STUB}{mine}"));
    assert_eq!(agents.lines().count(), mine.lines().count() + 3);
    assert!(!nested.join("CLAUDE.md").exists() && !nested.join("AGENTS.md").exists());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("CLAUDE.md") && text.contains("AGENTS.md"),
        "{text}"
    );
    // --json names each file and what was done
    std::fs::remove_file(pd.path().join("CLAUDE.md")).unwrap();
    let v = json_of(&rdy_at(&w, &nested, &["--json", "setup"]));
    let files = v["files"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert_eq!(files.len(), 2, "{v}");
    assert!(
        files[0]["path"].as_str().unwrap().ends_with("CLAUDE.md"),
        "{v}"
    );
    assert_eq!(files[0]["result"], "written", "{v}");
    assert!(
        files[1]["path"].as_str().unwrap().ends_with("AGENTS.md"),
        "{v}"
    );
    assert_eq!(files[1]["result"], "kept", "{v}");
    // outside every project no stub goes anywhere, and the rules project takes none either
    let meta = w.default_meta();
    assert!(is_project(&meta), "the first setup made it");
    let outside = short_tempdir();
    let out = rdy_at(&w, outside.path(), &["setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    no_stub_files(outside.path());
    no_stub_files(&meta);
    // an explicit project is the project
    let other = w.project();
    let elsewhere = short_tempdir();
    let p = other.path().to_str().unwrap();
    assert!(
        rdy_at(&w, elsewhere.path(), &["--project", p, "setup"])
            .status
            .success()
    );
    assert_eq!(read(&other.path().join("CLAUDE.md")), STUB);
    assert!(!elsewhere.path().join("CLAUDE.md").exists());
    // an explicit path that is no project is its own refusal
    let none = short_tempdir();
    let v = json_of(&rdy_at(
        &w,
        elsewhere.path(),
        &[
            "--json",
            "--project",
            none.path().to_str().unwrap(),
            "setup",
        ],
    ));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    assert!(!none.path().join("CLAUDE.md").exists());
}

#[test]
fn ain5_setup_is_idempotent_and_never_overwrites_a_link_target() {
    let w = World::new();
    let pd = w.project();
    let root = pd.path();
    std::fs::write(root.join("CLAUDE.md"), "Mine.\n").unwrap();
    assert!(rdy_at(&w, root, &["setup"]).status.success());
    let (c1, a1) = (read(&root.join("CLAUDE.md")), read(&root.join("AGENTS.md")));
    assert_eq!(c1, format!("{STUB}Mine.\n"));
    // a repeat leaves both files alone
    let v = json_of(&rdy_at(&w, root, &["--json", "setup"]));
    assert!(
        v["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["result"] == "kept"),
        "{v}"
    );
    assert_eq!(read(&root.join("CLAUDE.md")), c1);
    assert_eq!(read(&root.join("AGENTS.md")), a1);
    // a stub the person moved below their own text is still the stub: nothing is added
    let moved = format!("Mine first.\n\n{STUB}\nMore of mine.\n");
    std::fs::write(root.join("CLAUDE.md"), &moved).unwrap();
    assert!(rdy_at(&w, root, &["setup"]).status.success());
    assert_eq!(read(&root.join("CLAUDE.md")), moved);
    // a link is refused, before any file is written, and its target is untouched; the rules
    // project exists already, so the refusal adds nothing about it
    let target = short_tempdir();
    let t = target.path().join("shared.md");
    std::fs::write(&t, "Shared rules.\n").unwrap();
    let fresh = w.project();
    rdy_qa::platform::symlink(&t, &fresh.path().join("AGENTS.md")).unwrap();
    let out = rdy_at(&w, fresh.path(), &["--json", "setup"]);
    assert!(!out.status.success());
    let v = json_of(&out);
    let link = fresh.path().canonicalize().unwrap().join("AGENTS.md");
    assert_eq!(v["code"], "E_IO", "{v}");
    assert_eq!(
        v["message"].as_str().unwrap(),
        format!("{} is not a regular instruction file", link.display()),
        "{v}"
    );
    assert_eq!(v["fix"], NOT_REGULAR_FIX);
    assert_eq!(read(&t), "Shared rules.\n");
    assert!(
        !fresh.path().join("CLAUDE.md").exists(),
        "the preflight wrote nothing"
    );
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    // a directory in a target's place is refused the same way
    let dird = w.project();
    std::fs::create_dir(dird.path().join("CLAUDE.md")).unwrap();
    let v = json_of(&rdy_at(&w, dird.path(), &["--json", "setup"]));
    assert_eq!(v["code"], "E_IO", "{v}");
    assert_eq!(
        v["message"].as_str().unwrap(),
        format!(
            "{} is not a regular instruction file",
            dird.path()
                .canonicalize()
                .unwrap()
                .join("CLAUDE.md")
                .display()
        )
    );
    assert!(!dird.path().join("AGENTS.md").exists());
    // a failure after the first write says what was written; a repeat completes it once
    let half = w.project();
    let blocker = half.path().join(".AGENTS.md.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();
    let out = rdy_at(&w, half.path(), &["--json", "setup"]);
    assert!(!out.status.success());
    let v = json_of(&out);
    assert_eq!(v["code"], "E_IO", "{v}");
    let claude = half.path().canonicalize().unwrap().join("CLAUDE.md");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains(&format!("{} was written", claude.display())),
        "{v}"
    );
    assert_eq!(read(&claude), STUB);
    std::fs::remove_dir_all(&blocker).unwrap();
    assert!(rdy_at(&w, half.path(), &["setup"]).status.success());
    assert_eq!(read(&claude), STUB);
    assert_eq!(read(&half.path().join("AGENTS.md")), STUB);
}

#[test]
fn agi4_setup_creates_empty_meta_then_preserves_every_byte() {
    let w = World::new();
    let pd = w.project();
    let mine = "# My agents\n\nUse tabs.\nNo trailing newline here";
    std::fs::write(pd.path().join("AGENTS.md"), mine).unwrap();
    let meta = w.default_meta();
    assert!(!meta.exists());
    // the first setup: the rules project, then both stubs
    let nested = pd.path().join("a");
    std::fs::create_dir_all(&nested).unwrap();
    let v = ok_json(&w, &nested, &["setup"]);
    assert_eq!(v["meta"]["path"], meta.to_str().unwrap(), "{v}");
    assert_eq!(v["meta"]["result"], "created", "{v}");
    assert_eq!(v["meta"]["message"], created_message(&meta), "{v}");
    assert!(v["meta"].get("code").is_none(), "{v}");
    assert_eq!(v["stub"], json!({"result": "processed"}), "{v}");
    assert_eq!(v["warnings"], json!([]), "{v}");
    assert_eq!(v["files"][0]["result"], "written", "{v}");
    assert_eq!(v["files"][1]["result"], "written", "{v}");
    assert!(is_project(&meta));
    no_stub_files(&meta);
    assert_eq!(read(&pd.path().join("CLAUDE.md")), STUB);
    assert_eq!(read(&pd.path().join("AGENTS.md")), format!("{STUB}{mine}"));
    // the second setup keeps everything: the rules project by its message, the files by result,
    // every byte of both trees as they were
    let before = tree(&meta);
    let (c1, a1) = (
        read(&pd.path().join("CLAUDE.md")),
        read(&pd.path().join("AGENTS.md")),
    );
    let t = ok_text(&w, &nested, &["setup"]);
    assert!(t.contains(&kept_message(&meta)), "{t}");
    let v = ok_json(&w, &nested, &["setup"]);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    assert_eq!(v["meta"]["message"], kept_message(&meta), "{v}");
    assert!(
        v["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["result"] == "kept"),
        "{v}"
    );
    assert_eq!(tree(&meta), before, "the rules project is byte for byte");
    assert_eq!(read(&pd.path().join("CLAUDE.md")), c1);
    assert_eq!(read(&pd.path().join("AGENTS.md")), a1);
    // reading it shows no rule of the person and no copy of the guide: only the built-in layer
    let r = ok_json(&w, pd.path(), &["instructions"]);
    for key in ["instructions", "menu"] {
        assert!(
            r[key]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x["layer"] == "rdy"),
            "{r}"
        );
    }
}

#[test]
fn agi4_setup_makes_the_configured_absolute_path_not_the_default() {
    let w = World::new();
    let pd = w.project();
    let parent = short_tempdir();
    // a configured place that is not the default, with parents that do not exist yet
    let chosen = parent.path().join("rules").join("mine");
    w.person(&chosen);
    let v = ok_json(&w, pd.path(), &["setup"]);
    assert_eq!(v["meta"]["path"], chosen.to_str().unwrap(), "{v}");
    assert_eq!(v["meta"]["result"], "created", "{v}");
    assert_eq!(v["meta"]["message"], created_message(&chosen), "{v}");
    assert!(is_project(&chosen));
    assert!(!w.default_meta().exists());
    assert!(!w.home.path().join(".rdy").exists());
    let v = ok_json(&w, pd.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    assert_eq!(v["meta"]["path"], chosen.to_str().unwrap(), "{v}");
}

#[test]
fn agi4_setup_outside_a_project_writes_only_meta() {
    let w = World::new();
    let outside = short_tempdir();
    let meta = w.default_meta();
    let v = ok_json(&w, outside.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "created", "{v}");
    assert_eq!(
        v["stub"],
        json!({"result": "skipped", "reason": "outside_project"}),
        "{v}"
    );
    assert_eq!(v["files"], json!([]), "{v}");
    assert!(is_project(&meta));
    no_stub_files(outside.path());
    no_stub_files(&meta);
    let t = ok_text(&w, outside.path(), &["setup"]);
    assert_eq!(
        t,
        format!(
            "{}\nNo project is open. No agent instruction files were written.\n",
            kept_message(&meta)
        )
    );
    no_stub_files(outside.path());
    // a real project made here is the next thing to prepare: init says so, and setup does it
    let t = ok_text(&w, outside.path(), &["init"]);
    assert_eq!(t.lines().last().unwrap(), INIT_PROMPT, "{t}");
    let v = ok_json(&w, outside.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    assert_eq!(v["stub"], json!({"result": "processed"}), "{v}");
    assert_eq!(read(&outside.path().join("CLAUDE.md")), STUB);
    assert_eq!(read(&outside.path().join("AGENTS.md")), STUB);
    no_stub_files(&meta);
}

#[test]
fn agi4_setup_warnings_do_not_prevent_project_stubs() {
    let stubs_written = |v: &Value, pd: &Path| {
        assert_eq!(v["stub"], json!({"result": "processed"}), "{v}");
        assert_eq!(read(&pd.join("CLAUDE.md")), STUB);
        assert_eq!(read(&pd.join("AGENTS.md")), STUB);
        assert_eq!(v["meta"]["result"], "warning", "{v}");
    };

    // no home and no override: the message is exact, the stubs go in, outside a project nothing
    let w = World::without_home();
    let pd = w.project();
    let v = ok_json(&w, pd.path(), &["setup"]);
    stubs_written(&v, pd.path());
    assert_eq!(v["meta"]["path"], Value::Null, "{v}");
    assert_eq!(v["meta"]["message"], NO_HOME, "{v}");
    let t = ok_text(&w, pd.path(), &["setup"]);
    assert!(t.contains(NO_HOME), "{t}");
    let outside = short_tempdir();
    let v = ok_json(&w, outside.path(), &["setup"]);
    assert_eq!(v["meta"]["message"], NO_HOME, "{v}");
    assert_eq!(v["stub"]["reason"], "outside_project", "{v}");
    assert!(
        std::fs::read_dir(outside.path()).unwrap().next().is_none(),
        "no meta project and no file in the working directory"
    );

    // an override that is not absolute: its warning is forwarded, the default applies
    let w = World::new();
    w.config("instructions:\n  person: relative/rules\n");
    let pd = w.project();
    let v = ok_json(&w, pd.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "created", "{v}");
    assert_eq!(v["meta"]["path"], w.default_meta().to_str().unwrap(), "{v}");
    assert_eq!(v["stub"], json!({"result": "processed"}), "{v}");
    let warnings = v["warnings"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert!(!warnings.is_empty(), "{v}");
    for x in warnings {
        assert!(
            x["code"].as_str().unwrap().starts_with("W_")
                && x["message"].as_str().is_some()
                && x["fix"].as_str().is_some(),
            "{x}"
        );
    }
    let t = ok_text(&w, pd.path(), &["setup"]);
    assert!(t.contains("warning W_"), "{t}");

    // a regular file at the configured path
    let w = World::new();
    let pd = w.project();
    let hold = short_tempdir();
    let file = hold.path().join("rules.txt");
    std::fs::write(&file, "not a project").unwrap();
    w.person(&file);
    let v = ok_json(&w, pd.path(), &["setup"]);
    stubs_written(&v, pd.path());
    let message = format!(
        "Cannot create your rules project at {}: the path is not an rdy project.",
        file.display()
    );
    assert_eq!(v["meta"]["message"], message, "{v}");
    assert_eq!(v["meta"]["fix"], OCCUPIED_FIX, "{v}");
    assert_eq!(v["meta"]["code"], "E_NO_PROJECT", "{v}");
    assert_eq!(std::fs::read(&file).unwrap(), b"not a project");
    let t = ok_text(&w, pd.path(), &["setup"]);
    assert!(t.contains(&message) && t.contains(OCCUPIED_FIX), "{t}");

    // an empty directory at the configured path is occupied too
    let w = World::new();
    let pd = w.project();
    let dir = short_tempdir();
    w.person(dir.path());
    let v = ok_json(&w, pd.path(), &["setup"]);
    stubs_written(&v, pd.path());
    assert_eq!(
        v["meta"]["message"],
        format!(
            "Cannot create your rules project at {}: the path is not an rdy project.",
            dir.path().display()
        ),
        "{v}"
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());

    // a path under a regular file cannot be created: the cause ends the message
    let w = World::new();
    let pd = w.project();
    let hold = short_tempdir();
    let file = hold.path().join("plain");
    std::fs::write(&file, "x").unwrap();
    let under = file.join("rules");
    w.person(&under);
    let v = ok_json(&w, pd.path(), &["setup"]);
    stubs_written(&v, pd.path());
    let prefix = format!("Cannot create your rules project at {}: ", under.display());
    let m = v["meta"]["message"].as_str().unwrap();
    assert!(m.starts_with(&prefix) && m.len() > prefix.len(), "{v}");
    assert!(
        v["meta"]["code"]
            .as_str()
            .is_some_and(|c| c.starts_with("E_")),
        "{v}"
    );
    assert!(
        v["meta"]["fix"].as_str().is_some_and(|f| !f.is_empty()),
        "{v}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"x");
}

#[test]
fn agi4_setup_never_stubs_meta_or_its_alias() {
    let w = World::new();
    let pd = w.project();
    let meta = w.default_meta();
    ok_json(&w, pd.path(), &["setup"]);
    assert!(is_project(&meta));
    let skipped = |v: &Value| {
        assert_eq!(
            v["stub"],
            json!({"result": "skipped", "reason": "meta_project"}),
            "{v}"
        );
        assert_eq!(v["files"], json!([]), "{v}");
    };
    let elsewhere = short_tempdir();
    // an explicit meta project
    let v = ok_json(
        &w,
        elsewhere.path(),
        &["--project", meta.to_str().unwrap(), "setup"],
    );
    skipped(&v);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    let t = ok_text(
        &w,
        elsewhere.path(),
        &["--project", meta.to_str().unwrap(), "setup"],
    );
    assert!(
        t.contains("This is your rules project. No agent instruction files were written."),
        "{t}"
    );
    // a directory inside it
    let nested = meta.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    skipped(&ok_json(&w, &nested, &["setup"]));
    // a native alias of it: as the working directory, as the explicit project, and as the
    // configured place
    let alias = w.home.path().join("meta-link");
    rdy_qa::platform::symlink(&meta, &alias).unwrap();
    skipped(&ok_json(&w, &alias, &["setup"]));
    skipped(&ok_json(
        &w,
        elsewhere.path(),
        &["--project", alias.to_str().unwrap(), "setup"],
    ));
    w.person(&alias);
    skipped(&ok_json(&w, &meta, &["setup"]));
    // a worktree of it: the pointer names the project
    let wt = short_tempdir();
    std::fs::write(
        wt.path().join(".rdy-worktree"),
        format!("project: {}\nname: wt\n", meta.display()),
    )
    .unwrap();
    skipped(&ok_json(&w, wt.path(), &["setup"]));
    no_stub_files(wt.path());
    no_stub_files(&meta);
    no_stub_files(&nested);
}

#[test]
fn agi4_setup_keeps_finished_steps_after_stub_failure() {
    let w = World::new();
    let meta = w.default_meta();
    // a preflight failure: the rules project this run made stays, and the refusal says so
    let pd = w.project();
    std::fs::create_dir(pd.path().join("AGENTS.md")).unwrap();
    let out = rdy_at(&w, pd.path(), &["--json", "setup"]);
    assert!(!out.status.success());
    let v = json_of(&out);
    let agents = pd.path().canonicalize().unwrap().join("AGENTS.md");
    assert_eq!(v["code"], "E_IO", "{v}");
    assert_eq!(
        v["message"].as_str().unwrap(),
        format!(
            "{} is not a regular instruction file; {}",
            agents.display(),
            created_message(&meta)
        ),
        "{v}"
    );
    assert_eq!(v["fix"], NOT_REGULAR_FIX, "{v}");
    assert!(is_project(&meta), "the rules project survived the refusal");
    assert!(!pd.path().join("CLAUDE.md").exists());
    // the retry finds the rules project and completes both files
    std::fs::remove_dir(pd.path().join("AGENTS.md")).unwrap();
    let v = ok_json(&w, pd.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    assert_eq!(read(&pd.path().join("CLAUDE.md")), STUB);
    assert_eq!(read(&pd.path().join("AGENTS.md")), STUB);
    // a rules project that already existed is not named in a later refusal
    let other = w.project();
    std::fs::create_dir(other.path().join("CLAUDE.md")).unwrap();
    let v = json_of(&rdy_at(&w, other.path(), &["--json", "setup"]));
    assert!(!v["message"].as_str().unwrap().contains("Created"), "{v}");

    // a failure after the first file: that file stays, the refusal names it and the rules
    // project this run made
    let w = World::new();
    let meta = w.default_meta();
    let half = w.project();
    let blocker = half.path().join(".AGENTS.md.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();
    let out = rdy_at(&w, half.path(), &["--json", "setup"]);
    assert!(!out.status.success());
    let v = json_of(&out);
    let claude = half.path().canonicalize().unwrap().join("CLAUDE.md");
    let m = v["message"].as_str().unwrap();
    assert_eq!(v["code"], "E_IO", "{v}");
    assert!(
        m.contains(&format!("; {} was written", claude.display())),
        "{v}"
    );
    assert!(m.ends_with(&format!("; {}", created_message(&meta))), "{v}");
    assert!(is_project(&meta));
    assert_eq!(read(&claude), STUB);
    // the retry keeps the first file's bytes, writes the second and keeps the rules project
    std::fs::remove_dir_all(&blocker).unwrap();
    let before = tree(&meta);
    let v = ok_json(&w, half.path(), &["setup"]);
    assert_eq!(v["meta"]["result"], "kept", "{v}");
    assert_eq!(v["files"][0]["result"], "kept", "{v}");
    assert_eq!(v["files"][1]["result"], "written", "{v}");
    assert_eq!(read(&claude), STUB);
    assert_eq!(tree(&meta), before);
}

#[test]
fn agi4_init_and_status_prompts_follow_presence() {
    let w = World::new();
    let hold = short_tempdir();
    // a folder with the stub in none, one or both files (the second behind the person's text)
    let dir = |name: &str, claude: bool, agents: bool| -> PathBuf {
        let d = hold.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        if claude {
            std::fs::write(d.join("CLAUDE.md"), STUB).unwrap();
        }
        if agents {
            std::fs::write(d.join("AGENTS.md"), format!("Mine first.\n\n{STUB}")).unwrap();
        }
        d
    };
    // init in `d`: the JSON hint and the text's last line agree, and the result comes first
    let init = |w: &World, d: &Path, expected: Option<&str>| {
        let t = ok_text(w, d, &["init"]);
        assert!(t.starts_with("Initialized rdy project in "), "{t}");
        match expected {
            Some(p) => assert_eq!(t.lines().last().unwrap(), p, "{t}"),
            None => assert_eq!(t.lines().count(), 1, "{t}"),
        }
        std::fs::remove_dir_all(d.join(".rdy")).unwrap();
        std::fs::remove_file(d.join(".rdy.yaml")).unwrap();
        let v = ok_json(w, d, &["init"]);
        match expected {
            Some(p) => assert_eq!(v["setup_hint"], p, "{v}"),
            None => assert!(v.get("setup_hint").is_none(), "{v}"),
        }
    };
    // no rules project: even with both stubs the prompt stays
    init(&w, &dir("both-first", true, true), Some(INIT_PROMPT));
    // a project's status asks for the rules project while none exists, every time, and keeps no
    // first-run record anywhere
    let pd = w.project();
    for _ in 0..2 {
        let v = ok_json(&w, pd.path(), &["status"]);
        assert_eq!(v["setup_hint"], STATUS_PROMPT, "{v}");
        let t = ok_text(&w, pd.path(), &["status"]);
        assert!(t.lines().any(|l| l == STATUS_PROMPT), "{t}");
    }
    assert!(!w.default_meta().exists());
    assert!(!w.home.path().join(".rdy").exists());
    // setup makes the rules project: the status prompt goes
    ok_json(&w, pd.path(), &["setup"]);
    let v = ok_json(&w, pd.path(), &["status"]);
    assert!(v.get("setup_hint").is_none(), "{v}");
    let t = ok_text(&w, pd.path(), &["status"]);
    assert!(!t.contains(STATUS_PROMPT), "{t}");
    // with the rules project present, init prompts until both files hold the stub
    init(&w, &dir("both", true, true), None);
    init(&w, &dir("one", true, false), Some(INIT_PROMPT));
    init(&w, &dir("other-one", false, true), Some(INIT_PROMPT));
    init(&w, &dir("none", false, false), Some(INIT_PROMPT));
}

#[test]
fn agi4_init_of_the_meta_project_prints_no_prompt() {
    let w = World::new();
    let elsewhere = short_tempdir();
    let meta = w.default_meta();
    let t = ok_text(&w, elsewhere.path(), &["init", meta.to_str().unwrap()]);
    assert_eq!(t.lines().count(), 1, "{t}");
    assert!(!t.contains("rdy setup"), "{t}");
    no_stub_files(&meta);
    // a configured place is the same
    let w = World::new();
    let hold = short_tempdir();
    let chosen = hold.path().join("rules");
    w.person(&chosen);
    let v = ok_json(&w, elsewhere.path(), &["init", chosen.to_str().unwrap()]);
    assert!(v.get("setup_hint").is_none(), "{v}");
    // and an ordinary project made after it is prompted for its files
    let other = hold.path().join("other");
    let v = ok_json(&w, elsewhere.path(), &["init", other.to_str().unwrap()]);
    assert_eq!(v["setup_hint"], INIT_PROMPT, "{v}");
}
