//! ec12: `rdy clone <path|file> [<dir>] [--fork]`, the CLI twin of `rdy_clone` (D-9QV85,
//! D-D2NPN, D-KQ4CA). A project directory clones through its daemon (or in process with
//! `--no-daemon`); a dump file clones in this process, as `rdy load` runs; a URL is the sync
//! client's (O7). The target is `dir`, else `--project`, else the current directory (O38).
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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
    let mut cmd = rdy_qa::fixtures::rdy_command();
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
    // a URL that is not HTTPS gets rdy remote add's refusal (D-47HVF)
    for url in ["file:///nowhere/p.rdydump", "rdy://hub/team/project"] {
        let out = human_command(scratch.path(), &env, &["--json", "clone", url, to_arg])
            .output()
            .unwrap();
        let (ok, e) = parsed(&out);
        assert!(!ok, "{url}: {e}");
        assert_eq!(e["code"], "E_BAD_INPUT", "{url}: {e}");
        assert_eq!(e["message"], "the hub address is not an HTTPS URL", "{e}");
        assert_eq!(
            e["fix"], "use an HTTPS address without credentials, a query or a fragment",
            "{e}"
        );
        assert!(!to.exists(), "{url}: the target is untouched");
    }
    // an https URL is a hub clone, which never forks: refused before any request
    let out = human_command(
        scratch.path(),
        &env,
        &[
            "--json",
            "clone",
            "https://127.0.0.1:9/acme/docs",
            to_arg,
            "--fork",
        ],
    )
    .output()
    .unwrap();
    let (ok, e) = parsed(&out);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"], "a fork starts from a project or a dump, not from a hub URL",
        "{e}"
    );
    assert_eq!(
        e["fix"], "clone the hub repo, then run rdy clone --fork <that project> <dir>",
        "{e}"
    );
    assert!(!to.exists(), "the target is untouched");

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

/// hfp3: `golden_served`, then one more landing tagged `v2.0.0`; its repo id and the landing of
/// `v1.0.0`, which is not canon's head.
fn two_tag_served() -> (Served, String, String) {
    let (s, id) = golden_served();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    rdy_qa::golden::small_site(&mut r);
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let st = r.read();
    let v1 = st
        .versions()
        .unwrap()
        .into_iter()
        .find(|v| v.name == "v1.0.0")
        .expect("v1.0.0")
        .landing;
    assert_ne!(
        st.canon_head().map(|h| h.to_string()),
        Some(v1.clone()),
        "v1.0.0 is behind canon"
    );
    drop(st);
    r.close().unwrap();
    (s, id, v1)
}

/// Every path under `dir` with its bytes; empty when `dir` does not exist.
fn files_of(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = vec![];
    if !dir.exists() {
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                stack.push(p);
                out.push((rel, None));
            } else {
                out.push((rel, Some(std::fs::read(&p).unwrap())));
            }
        }
    }
    out.sort();
    out
}

/// The code, message and fix of a refused CLI run.
fn cli_refusal(out: &Output) -> (String, String, String) {
    let (ok, e) = parsed(out);
    assert!(!ok, "{e}");
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// The code, message and fix of a refused MCP call.
fn mcp_refusal(r: rdy_qa::mcp::Reply) -> (String, String, String) {
    let rdy_qa::mcp::Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

#[test]
fn hfp3_at_refusals_match_on_all_surfaces() {
    let (s, _id, _v1) = two_tag_served();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let src = s.dir.path().to_str().unwrap().to_string();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap().to_string();
    stdout(&s.rdy(&["dump", "--out", &file_arg]));
    s.stop();
    assert!(s.daemon_pid().is_none());
    let to = scratch.path().join("to");
    let to_arg = to.to_str().unwrap().to_string();
    let full = scratch.path().join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("keep.txt"), b"keep").unwrap();
    let full_arg = full.to_str().unwrap().to_string();
    const URL: &str = "https://127.0.0.1:9/acme/docs";

    // each row: the CLI arguments after `clone`, the MCP arguments, the target, and the
    // expected refusal (the message and fix of a bad tag name are the parser's)
    struct Row {
        cli: Vec<String>,
        mcp: Value,
        target: PathBuf,
        want: (&'static str, Option<String>, Option<String>),
    }
    let own = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let rows = vec![
        Row {
            cli: own(&[&src, &to_arg, "--at", "v1.0.0"]),
            mcp: json!({"dir": &to_arg, "at": "v1.0.0"}),
            target: to.clone(),
            want: (
                "E_BAD_INPUT",
                Some("a clone keeps the whole history; --at needs --fork".into()),
                Some("add --fork, or remove --at".into()),
            ),
        },
        Row {
            cli: own(&[&file_arg, &to_arg, "--at", "v1.0.0"]),
            mcp: json!({"dir": &to_arg, "file": &file_arg, "at": "v1.0.0"}),
            target: to.clone(),
            want: (
                "E_BAD_INPUT",
                Some("a clone keeps the whole history; --at needs --fork".into()),
                Some("add --fork, or remove --at".into()),
            ),
        },
        Row {
            cli: own(&[&file_arg, &to_arg, "--fork", "--at", "v1.0.0"]),
            mcp: json!({"dir": &to_arg, "file": &file_arg, "fork": true, "at": "v1.0.0"}),
            target: to.clone(),
            want: (
                "E_BAD_INPUT",
                Some("--at needs a project".into()),
                Some("load the dump, then fork the loaded project".into()),
            ),
        },
        Row {
            cli: own(&[URL, &to_arg, "--fork", "--at", "v1.0.0"]),
            mcp: json!({"dir": &to_arg, "url": URL, "fork": true, "at": "v1.0.0"}),
            target: to.clone(),
            want: (
                "E_BAD_INPUT",
                Some("a fork starts from a project or a dump, not from a hub URL".into()),
                Some("clone the hub repo, then run rdy clone --fork <that project> <dir>".into()),
            ),
        },
        Row {
            cli: own(&[URL, &to_arg, "--at", "v1.0.0"]),
            mcp: json!({"dir": &to_arg, "url": URL, "at": "v1.0.0"}),
            target: to.clone(),
            want: (
                "E_BAD_INPUT",
                Some("a clone keeps the whole history; --at needs --fork".into()),
                Some("add --fork, or remove --at".into()),
            ),
        },
        Row {
            cli: own(&[&src, &to_arg, "--fork", "--at", "v9.9.9"]),
            mcp: json!({"dir": &to_arg, "fork": true, "at": "v9.9.9"}),
            target: to.clone(),
            want: (
                "E_NOT_FOUND",
                Some("no version v9.9.9".into()),
                Some("list versions with rdy tags".into()),
            ),
        },
        Row {
            cli: own(&[&src, &to_arg, "--fork", "--at", "latest"]),
            mcp: json!({"dir": &to_arg, "fork": true, "at": "latest"}),
            target: to.clone(),
            want: ("E_BAD_TAG_NAME", None, None),
        },
        Row {
            cli: own(&[&src, &full_arg, "--fork", "--at", "v1.0.0"]),
            mcp: json!({"dir": &full_arg, "fork": true, "at": "v1.0.0"}),
            target: full.clone(),
            want: (
                "E_ALREADY_EXISTS",
                Some(format!("{} is not empty", full.display())),
                Some("fork into a new directory, or an empty one".into()),
            ),
        },
    ];

    let mut seen: Vec<Vec<(String, String, String)>> = vec![vec![]; rows.len()];
    let check = |i: usize, surface: &str, got: (String, String, String)| {
        let r = &rows[i];
        assert_eq!(got.0, r.want.0, "row {i} {surface}: {got:?}");
        if let Some(m) = &r.want.1 {
            assert_eq!(&got.1, m, "row {i} {surface}");
        }
        if let Some(f) = &r.want.2 {
            assert_eq!(&got.2, f, "row {i} {surface}");
        }
        got
    };
    // --no-daemon first: the source opens in this process while no daemon holds it
    for (i, r) in rows.iter().enumerate() {
        let before = files_of(&r.target);
        let mut args = own(&["--no-daemon", "--json", "clone"]);
        args.extend(r.cli.iter().cloned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = human_command(scratch.path(), &env, &args).output().unwrap();
        seen[i].push(check(i, "--no-daemon", cli_refusal(&out)));
        assert_eq!(
            files_of(&r.target),
            before,
            "row {i}: the target is untouched"
        );
    }
    assert!(s.daemon_pid().is_none(), "--no-daemon starts no daemon");
    // through the daemon
    for (i, r) in rows.iter().enumerate() {
        let before = files_of(&r.target);
        let mut args = own(&["--json", "clone"]);
        args.extend(r.cli.iter().cloned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = human_command(scratch.path(), &env, &args).output().unwrap();
        seen[i].push(check(i, "daemon", cli_refusal(&out)));
        assert_eq!(
            files_of(&r.target),
            before,
            "row {i}: the target is untouched"
        );
    }
    // the agent's tool
    let mut c = rdy_qa::mcp::McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    for (i, r) in rows.iter().enumerate() {
        let before = files_of(&r.target);
        seen[i].push(check(
            i,
            "mcp",
            mcp_refusal(c.call("rdy_clone", r.mcp.clone())),
        ));
        assert_eq!(
            files_of(&r.target),
            before,
            "row {i}: the target is untouched"
        );
    }
    drop(c);
    for (i, got) in seen.iter().enumerate() {
        assert_eq!(got.len(), 3, "row {i}");
        assert!(
            got.iter().all(|g| g == &got[0]),
            "row {i}: the three surfaces give one refusal: {got:?}"
        );
    }
    assert!(!to.exists(), "no row made the target");
    s.stop();
}

#[test]
fn hfp3_rdy_clone_fork_at_prints_forked_from() {
    let (s, id, v1) = two_tag_served();
    let scratch = short_tempdir();
    let env = s.env_pairs();
    let src = s.dir.path().to_str().unwrap();

    // --no-daemon, then through the daemon: the text names the tag
    for (name, lead) in [("a", vec!["--no-daemon"]), ("b", vec![])] {
        let to = scratch.path().join(name);
        let to_arg = to.to_str().unwrap();
        let mut args = lead.clone();
        args.extend(["clone", "--fork", src, to_arg, "--at", "v1.0.0"]);
        let out = human_command(scratch.path(), &env, &args).output().unwrap();
        let text = stdout(&out);
        let new_id = repo_of(&s, &to);
        assert_ne!(new_id, id, "a fork takes a new id: {text}");
        check_text(
            &text,
            to_arg,
            &new_id,
            &format!("forked from {id} at version v1.0.0"),
        );
        assert!(to.join(".rdy.yaml").is_file(), "{text}");
        assert!(!to.join(".rdy/local.yaml").exists(), "{text}");
    }
    assert!(
        s.daemon_pid().is_some(),
        "the second fork went through the daemon"
    );

    // --json: forked_from names the tag and its landing, never the newest tag
    let c = scratch.path().join("c");
    let out = human_command(
        scratch.path(),
        &env,
        &[
            "--json",
            "clone",
            src,
            c.to_str().unwrap(),
            "--fork",
            "--at",
            "v1.0.0",
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
    assert_eq!(v["forked_from"]["landing"], v1.as_str(), "{v}");
    assert_eq!(v["tags"], 1, "only v1.0.0 comes along: {v}");
    assert!(v.get("origin").is_none(), "{v}");
    assert_eq!(v["fsck"]["unlogged_moves"], 0, "{v}");
    s.stop();
}
