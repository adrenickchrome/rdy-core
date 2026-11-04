//! ain5 (D-8X04F): `rdy setup` writes the three-line stub into `CLAUDE.md` and `AGENTS.md` of the
//! chosen project root, or of the current directory when there is no project; never into the
//! meta project because it supplies the rules. The stub goes first, before the person's own
//! text, which stays as it was. A repeat changes nothing. A link or any other file that is not a
//! regular file is refused before either file is written, and its target is never touched.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within;
use rdy_qa::instructions::World;
use serde_json::Value;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

const NOT_REGULAR_FIX: &str = "replace it with a regular file, then run rdy setup again";

/// `rdy <args>` in `cwd`, with no `--project` unless `args` names one, as the world's human.
fn rdy_at(w: &World, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
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

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
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
    // outside every project: the current directory, never the meta project
    let meta = w.default_meta();
    w.project_at(&meta);
    let outside = short_tempdir();
    let out = rdy_at(&w, outside.path(), &["setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read(&outside.path().join("CLAUDE.md")), STUB);
    assert_eq!(read(&outside.path().join("AGENTS.md")), STUB);
    assert!(!meta.join("CLAUDE.md").exists() && !meta.join("AGENTS.md").exists());
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
    // a link is refused, before any file is written, and its target is untouched
    let target = short_tempdir();
    let t = target.path().join("shared.md");
    std::fs::write(&t, "Shared rules.\n").unwrap();
    let fresh = short_tempdir();
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
    let dird = short_tempdir();
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
    let half = short_tempdir();
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
