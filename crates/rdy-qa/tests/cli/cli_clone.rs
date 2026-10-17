//! ec12: `rdy clone <path|file> [<dir>] [--fork]`, the CLI twin of `rdy_clone` (D-9QV85,
//! D-D2NPN, D-KQ4CA). A project directory clones through its daemon (or in process with
//! `--no-daemon`); a dump file clones in this process, as `rdy load` runs; a URL is the sync
//! client's (O7). The target is `dir`, else `--project`, else the current directory (O38).
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

/// The `next` line of every clone (O9).
const NEXT: &str = "next: run rdy checkout to bind a working copy; a clone carries none";

/// A served project holding the golden corpus, landed and tagged `v1.0.0`, and its repo id.
fn golden_served() -> (Served, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let id = r.read().repo_id().unwrap().to_string();
    r.close().unwrap();
    (s, id)
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether the command succeeded, and its JSON: stdout on success, stderr on failure.
fn parsed(out: &Output) -> (bool, Value) {
    let ok = out.status.success();
    let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
    (ok, v)
}

fn is_count(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `cloned {batches} batches and {frames} frames into {project}: {l} landings, {t} versions, {v} views; fsck ok`.
fn is_the_clone_line(line: &str, project: &str) -> bool {
    let Some(rest) = line.strip_prefix("cloned ") else {
        return false;
    };
    let Some((batches, rest)) = rest.split_once(" batches and ") else {
        return false;
    };
    let Some((frames, rest)) = rest.split_once(" frames into ") else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix(project)
        .and_then(|r| r.strip_prefix(": "))
    else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(" views; fsck ok") else {
        return false;
    };
    let Some((l, rest)) = rest.split_once(" landings, ") else {
        return false;
    };
    let Some((t, v)) = rest.split_once(" versions, ") else {
        return false;
    };
    [batches, frames, l, t, v].iter().all(|x| is_count(x))
}

/// The four lines of a clone's text: the clone line, `repo <id>`, the origin line, `next: …`.
fn check_text(text: &str, project: &str, repo: &str, origin: &str) {
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text:?}");
    assert!(is_the_clone_line(lines[0], project), "{text:?}");
    assert_eq!(lines[1], format!("repo {repo}"), "{text:?}");
    assert_eq!(lines[2], origin, "{text:?}");
    assert_eq!(lines[3], NEXT, "{text:?}");
}

/// `rdy <args>` run in `cwd` as a person, as `human_command` builds it but without its
/// `--project`.
fn bare(cwd: &Path, env: &[(String, String)], args: &[&str]) -> Command {
    let mut cmd = Command::new(rdy_qa::fixtures::rdy_bin());
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd);
    cmd
}

/// The repo id `rdy --json status` names in `dir` (in process, so no daemon starts there).
fn repo_of(s: &Served, dir: &Path) -> String {
    let out = human_command(dir, &s.env_pairs(), &["--no-daemon", "--json", "status"])
        .output()
        .unwrap();
    let (ok, v) = parsed(&out);
    assert!(ok, "{v}");
    v["repo_id"].as_str().unwrap().to_string()
}

#[test]
fn ec12_rdy_clone_of_a_project_path_goes_through_its_daemon() {
    let (s, id) = golden_served();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let src = s.dir.path().to_str().unwrap();
    assert!(s.daemon_pid().is_none(), "no daemon before the clone");

    // --no-daemon: the source opens in this process, and no daemon starts
    let a = scratch.path().join("a");
    let a_arg = a.to_str().unwrap();
    let out = human_command(scratch.path(), &env, &["--no-daemon", "clone", src, a_arg])
        .output()
        .unwrap();
    check_text(
        &stdout(&out),
        a_arg,
        &id,
        &format!("cloned from {id} at version v1.0.0"),
    );
    assert!(s.daemon_pid().is_none(), "--no-daemon starts no daemon");
    assert!(a.join(".rdy/store").is_dir());

    // through the daemon: the clone starts the source's daemon, which serves the op
    let b = scratch.path().join("b");
    let b_arg = b.to_str().unwrap();
    let out = human_command(scratch.path(), &env, &["clone", src, b_arg])
        .output()
        .unwrap();
    check_text(
        &stdout(&out),
        b_arg,
        &id,
        &format!("cloned from {id} at version v1.0.0"),
    );
    assert!(
        s.daemon_pid().is_some(),
        "the clone went through the source's daemon"
    );
    assert!(
        rdy_daemon::live_daemon(&b).is_none(),
        "no daemon serves the clone"
    );

    // --json: the reply carries the repo id and the origin, and the clone keeps the id
    let c = scratch.path().join("c");
    let out = human_command(
        scratch.path(),
        &env,
        &["--json", "clone", src, c.to_str().unwrap()],
    )
    .output()
    .unwrap();
    let (ok, v) = parsed(&out);
    assert!(ok, "{v}");
    assert_eq!(v["repo_id"], id.as_str(), "{v}");
    assert_eq!(v["fork"], false, "{v}");
    assert_eq!(v["origin"]["repo_id"], id.as_str(), "{v}");
    assert_eq!(v["origin"]["tag"], "v1.0.0", "{v}");
    assert!(v.get("forked_from").is_none(), "{v}");
    assert_eq!(v["fsck"]["unlogged_moves"], 0, "{v}");
    assert_eq!(repo_of(&s, &c), id);
    s.stop();
}

#[test]
fn ec12_rdy_clone_of_a_dump_file_runs_in_process() {
    let (s, id) = golden_served();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap();
    stdout(&s.rdy(&["dump", "--out", file_arg]));
    s.stop();
    assert!(s.daemon_pid().is_none());

    // the target is `dir`
    let a = scratch.path().join("a");
    let a_arg = a.to_str().unwrap();
    let out = human_command(scratch.path(), &env, &["clone", file_arg, a_arg])
        .output()
        .unwrap();
    check_text(
        &stdout(&out),
        a_arg,
        &id,
        &format!("cloned from {id} at version v1.0.0"),
    );
    assert!(s.daemon_pid().is_none(), "a file clone asks no daemon");
    assert!(
        rdy_daemon::live_daemon(&a).is_none(),
        "no daemon serves the clone"
    );
    assert_eq!(repo_of(&s, &a), id);

    // with no dir: --project
    let b = scratch.path().join("b");
    let b_arg = b.to_str().unwrap();
    let out = bare(
        scratch.path(),
        &env,
        &["--project", b_arg, "clone", file_arg],
    )
    .output()
    .unwrap();
    check_text(
        &stdout(&out),
        b_arg,
        &id,
        &format!("cloned from {id} at version v1.0.0"),
    );
    assert!(b.join(".rdy/store").is_dir());

    // with neither: the current directory
    let c = scratch.path().join("c");
    std::fs::create_dir(&c).unwrap();
    let out = bare(&c, &env, &["clone", file_arg]).output().unwrap();
    let text = stdout(&out);
    assert!(text.starts_with("cloned "), "{text:?}");
    assert!(c.join(".rdy/store").is_dir(), "{text:?}");
    assert!(c.join(".rdy.yaml").is_file(), "{text:?}");
    assert_eq!(repo_of(&s, &c), id);
}

#[test]
fn ec12_rdy_clone_fork_prints_forked_from() {
    let (s, id) = golden_served();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap();
    stdout(&s.rdy(&["dump", "--out", file_arg]));
    let src = s.dir.path().to_str().unwrap();

    for (name, source) in [("from-file", file_arg), ("from-project", src)] {
        let to = scratch.path().join(name);
        let to_arg = to.to_str().unwrap();
        let out = human_command(scratch.path(), &env, &["clone", "--fork", source, to_arg])
            .output()
            .unwrap();
        let text = stdout(&out);
        let new_id = repo_of(&s, &to);
        assert_ne!(new_id, id, "a fork takes a new id: {text}");
        check_text(
            &text,
            to_arg,
            &new_id,
            &format!("forked from {id} at version v1.0.0"),
        );

        let json_to = scratch.path().join(format!("{name}-json"));
        let out = human_command(
            scratch.path(),
            &env,
            &[
                "--json",
                "clone",
                source,
                json_to.to_str().unwrap(),
                "--fork",
            ],
        )
        .output()
        .unwrap();
        let (ok, v) = parsed(&out);
        assert!(ok, "{v}");
        assert_eq!(v["fork"], true, "{v}");
        assert_ne!(v["repo_id"], id.as_str(), "{v}");
        assert_eq!(v["forked_from"]["repo_id"], id.as_str(), "{v}");
        assert_eq!(v["forked_from"]["tag"], "v1.0.0", "{v}");
        assert!(v.get("origin").is_none(), "{v}");
    }
    s.stop();
}

#[test]
fn ec12_a_url_source_names_the_sync_client() {
    let s = Served::new();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let to = scratch.path().join("to");
    let to_arg = to.to_str().unwrap();
    for url in ["file:///nowhere/p.rdydump", "rdy://hub/team/project"] {
        let out = human_command(scratch.path(), &env, &["--json", "clone", url, to_arg])
            .output()
            .unwrap();
        let (ok, e) = parsed(&out);
        assert!(!ok, "{url}: {e}");
        assert_eq!(e["code"], "E_BAD_INPUT", "{url}: {e}");
        assert_eq!(
            e["message"],
            format!("{url} is a URL: cloning from a hub comes with the sync client"),
            "{e}"
        );
        assert_eq!(
            e["fix"], "run rdy dump --out <file> in that project, then clone the file",
            "{e}"
        );
        assert!(!to.exists(), "{url}: the target is untouched");
    }

    // a directory that is no project is refused too (O7, closed)
    let plain = scratch.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let out = human_command(
        scratch.path(),
        &env,
        &["--json", "clone", plain.to_str().unwrap(), to_arg],
    )
    .output()
    .unwrap();
    let (ok, e) = parsed(&out);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"],
        format!(
            "{} is neither a dump file nor an rdy project",
            plain.display()
        ),
        "{e}"
    );
    assert_eq!(
        e["fix"], "pass a dump file, or a directory with .rdy.yaml and .rdy/store",
        "{e}"
    );
    assert!(!to.exists(), "the target is untouched");
    assert!(s.daemon_pid().is_none(), "no daemon was asked");
}
