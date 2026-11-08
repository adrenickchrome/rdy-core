//! cfgl1: settings come from four levels (FW-X5, D-C5955): .rdy/local.yaml,
//! the project config (.rdy.yaml), the user config and the defaults; the most
//! specific wins; a key at the wrong level is a W_CONFIG_UNKNOWN warning naming the right file,
//! and rdy ignores it; rdy config --show-origin prints each value's file.
use rdy_engine::config_levels::*;
use rdy_engine::*;
use rdy_qa::daemon::Served;
use std::path::{Path, PathBuf};

fn proj() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let p = d.path().to_path_buf();
    (d, p)
}

fn user(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    std::fs::write(&f, text).unwrap();
    (d, f)
}

fn append(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    let mut t = std::fs::read_to_string(&f).unwrap_or_default();
    t.push_str(text);
    std::fs::write(f, t).unwrap();
}

#[test]
fn cfgl1_the_most_specific_level_wins() {
    let (_d, p) = proj();
    let (_u, uf) = user("actor: human:me\nviews:\n  idle_days: 3\nserve:\n  idle_timeout: 100\n");
    append(&p, PROJECT_FILE, "serve:\n  idle_timeout: 200\n");
    append(
        &p,
        LOCAL_FILE,
        "serve:\n  idle_timeout: 300\nactor: human:here\n",
    );
    let e = effective(&p, Some(&uf)).unwrap();
    let get = |k: &str| {
        e.origins
            .iter()
            .find(|o| o.key == k)
            .unwrap_or_else(|| panic!("{k}"))
    };
    assert_eq!(
        get("serve.idle_timeout").value,
        serde_yaml_ng::Value::from(300u64)
    );
    assert_eq!(get("serve.idle_timeout").level, Level::Local);
    assert_eq!(get("views.idle_days").level, Level::User);
    assert_eq!(get("actor").value.as_str(), Some("human:here"));
    assert_eq!(get("serve.start_timeout").level, Level::Default);
    assert_eq!(get("pack").level, Level::Project);
    assert_eq!(
        serve_config(&p).idle_timeout,
        std::time::Duration::from_secs(300)
    );
    assert_eq!(
        human_actor_for(&p, Some(&uf), Some("u")).unwrap(),
        "human:here"
    );
    let fp = pack_fingerprint(&p).unwrap();
    assert!(
        fp.files.iter().any(|(f, _)| f.ends_with(LOCAL_FILE)),
        "a local change reloads like a config change"
    );
}

#[test]
fn cfgl1_a_key_at_the_wrong_level_warns_naming_the_right_file() {
    let (_d, p) = proj();
    append(&p, PROJECT_FILE, "actor: human:all\n");
    let e = effective(&p, None).expect("a wrong-file key is a warning");
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert!(
        e.warnings[0].message.contains("actor")
            && e.warnings[0].fix.contains(".rdy/local.yaml")
            && e.warnings[0].fix.contains("user config"),
        "{:?}",
        e.warnings[0]
    );
    assert!(Rdy::open(&p).is_ok(), "open accepts it too");
    let (_d, p) = proj();
    append(&p, LOCAL_FILE, "pack: other\n");
    let e = effective(&p, None).expect("a wrong-file key is a warning");
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0].message.contains("pack") && e.warnings[0].fix.contains(".rdy.yaml"),
        "{:?}",
        e.warnings[0]
    );
    assert!(Rdy::open(&p).is_ok(), "open accepts it too");
    let (_d, p) = proj();
    let (_u, uf) = user("decisions:\n  home: x\n");
    let e = effective(&p, Some(&uf)).expect("a wrong-file key is a warning");
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0].message.contains("decisions.home") && e.warnings[0].fix.contains(".rdy.yaml"),
        "{:?}",
        e.warnings[0]
    );
}

#[test]
fn cfgl1_rdy_config_show_origin() {
    let s = Served::new();
    append(s.dir.path(), LOCAL_FILE, "views:\n  idle_days: 9\n");
    let (okk, v) = s.json(&["--json", "config", "--show-origin"]);
    assert!(okk, "{v}");
    let rows = v.as_array().unwrap();
    let row = rows.iter().find(|r| r["key"] == "views.idle_days").unwrap();
    assert_eq!(row["value"], 9);
    assert_eq!(row["level"], "local");
    let local_file = s.dir.path().join(".rdy").join("local.yaml");
    let local_text = local_file.display().to_string();
    assert_eq!(row["file"].as_str(), Some(local_text.as_str()), "{row}");
    let actor = rows.iter().find(|r| r["key"] == "actor").unwrap();
    assert_eq!(actor["value"], "human:qa");
    assert_eq!(actor["level"], "user");
    let text = String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l
                == format!("views.idle_days = 9  # {local_text}  # set at: local, project, user")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == "serve.start_timeout = 30  # default  # set at: local, project, user"),
        "{text}"
    );
    // cc17: coverage.target is a project key with the default 80
    assert!(
        text.lines()
            .any(|l| l == "coverage.target = 80  # default  # set at: project"),
        "{text}"
    );
}

#[test]
fn dgm5_config_cli_lists_the_project_origin() {
    let s = Served::new();
    // the key lists in KEY_LEVELS order, with its default and then its project origin
    let in_table_order = |rows: &[serde_json::Value]| {
        let at: Vec<usize> = rows
            .iter()
            .filter_map(|r| {
                KEY_LEVELS
                    .iter()
                    .position(|k| Some(k.key) == r["key"].as_str())
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    };
    let (okk, v) = s.json(&["--json", "config", "--show-origin"]);
    assert!(okk, "{v}");
    let rows = v.as_array().unwrap();
    in_table_order(rows);
    let row = rows.iter().find(|r| r["key"] == "site.base_path").unwrap();
    assert_eq!(row["value"], "/");
    assert_eq!(row["level"], "default");
    assert!(row["file"].is_null(), "{row}");
    let text = String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l == "site.base_path = /  # default  # set at: project"),
        "{text}"
    );

    append(s.dir.path(), PROJECT_FILE, "site:\n  base_path: /docs/\n");
    let (okk, v) = s.json(&["--json", "config", "--show-origin"]);
    assert!(okk, "{v}");
    let rows = v.as_array().unwrap();
    in_table_order(rows);
    let row = rows.iter().find(|r| r["key"] == "site.base_path").unwrap();
    assert_eq!(row["value"], "/docs/");
    assert_eq!(row["level"], "project");
    assert!(
        row["file"].as_str().unwrap().ends_with(".rdy.yaml"),
        "{row}"
    );
    let keys: Vec<&str> = rows.iter().map(|r| r["key"].as_str().unwrap()).collect();
    let at = |k: &str| keys.iter().position(|x| *x == k).unwrap();
    assert!(at("pack") < at("site.base_path"));
    assert!(at("site.base_path") < at("reviews.require_up_to_date"));
    let text = String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l.starts_with("site.base_path = /docs/  # ")
                && l.ends_with(".rdy.yaml  # set at: project")),
        "{text}"
    );
    // the same effective value without the origin column
    let text = String::from_utf8_lossy(&s.rdy(&["config"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l == "site.base_path = /docs/  # set at: project"),
        "{text}"
    );
    // text and JSON list the same keys in the same order
    let text = String::from_utf8_lossy(&s.rdy(&["config"]).stdout).into_owned();
    let text_keys: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_once(" = ").map(|(k, _)| k))
        .collect();
    assert_eq!(text_keys, keys);
}

#[test]
fn cfgl1_a_local_actor_makes_that_actor_the_human_here() {
    let s = Served::new();
    append(s.dir.path(), LOCAL_FILE, "actor: human:alias\n");
    let o = s.rdy_as("human:alias", None, &["fsck", "--clean-quarantine"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = s.rdy_as("human:qa", None, &["--json", "fsck", "--clean-quarantine"]);
    assert!(
        !o.status.success(),
        "the user-level actor is no longer the human in this project"
    );
}

#[test]
fn cfgl1_a_wrong_typed_value_names_the_file_it_is_in() {
    let (_d, p) = proj();
    append(&p, LOCAL_FILE, "serve:\n  idle_timeout: soon\n");
    let e = effective(&p, None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message.contains("serve.idle_timeout") && e.message.contains(".rdy/local.yaml"),
        "{e}"
    );
    assert!(
        e.fix.contains(".rdy/local.yaml") && !e.fix.contains(PROJECT_FILE),
        "{e}"
    );
    let e = Rdy::open(&p).err().expect("open refuses it");
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message.contains(".rdy/local.yaml") && e.fix.contains(".rdy/local.yaml"),
        "{e}"
    );
}

#[test]
fn cfgw1_a_typo_warns_in_config_show_origin_status_and_every_command() {
    let s = Served::new();
    append(s.dir.path(), PROJECT_FILE, "decision:\n  home: docs\n");
    let text = String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l.starts_with("warning W_CONFIG_UNKNOWN: ")
                && l.contains("decision.home")
                && l.contains("decisions.home")),
        "{text}"
    );
    let o = s.rdy(&["status"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out.contains("warning: W_CONFIG_UNKNOWN") && out.contains("decision.home"),
        "{out}"
    );
    // another command prints it once on stderr and keeps stdout clean for --json
    let o = s.rdy(&["--json", "tags"]);
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(err.matches("W_CONFIG_UNKNOWN").count(), 1, "{err}");
    serde_json::from_slice::<serde_json::Value>(&o.stdout).expect("stdout is still JSON");
    // a project with only known keys prints none
    let s = Served::new();
    let o = s.rdy(&["--json", "tags"]);
    assert!(!String::from_utf8_lossy(&o.stderr).contains("W_CONFIG_UNKNOWN"));
}

#[test]
fn cfgw1_rdy_session_carries_the_warning() {
    use rdy_qa::mcp::McpClient;
    let s = Served::new();
    append(s.dir.path(), PROJECT_FILE, "decision:\n  home: docs\n");
    let mut c = McpClient::start_proxy(&s);
    let r = c
        .call(
            "rdy_session",
            serde_json::json!({"actor": "agent:m", "role": "writer"}),
        )
        .ok();
    let w = &r["warnings"][0];
    assert_eq!(w["code"], "W_CONFIG_UNKNOWN", "{r}");
    assert!(
        w["message"].as_str().unwrap().contains("decision.home"),
        "{r}"
    );
    assert!(w["fix"].as_str().unwrap().contains("decisions.home"), "{r}");
}

/// Guard: the user config is validated like the project's. One that does not parse, or is not a
/// mapping, is E_CONFIG naming it, for every project, never skipped (a skipped file would change
/// who the human is without a word).
#[test]
fn cfgl1_a_user_config_that_does_not_parse_is_e_config_naming_it() {
    let (_d, p) = proj();
    for text in ["actor: [human:me\n", "- actor\n"] {
        let (_u, uf) = user(text);
        let e = effective(&p, Some(&uf)).unwrap_err();
        assert_eq!(e.code, E_CONFIG, "{e}");
        let shown = uf.display().to_string();
        assert!(e.message.contains(&shown) || e.fix.contains(&shown), "{e}");
    }
}

#[test]
fn msw19_a_dotted_key_that_is_not_a_review_setting_warns() {
    let (_d, p) = proj();
    append(&p, PROJECT_FILE, "serve.idle_timeout: 5\n");
    let e = effective(&p, None).unwrap();
    let w: Vec<_> = e
        .warnings
        .iter()
        .filter(|w| w.code == "W_CONFIG_UNKNOWN")
        .collect();
    assert_eq!(w.len(), 1, "{:?}", e.warnings);
    assert!(
        w[0].message.contains("serve.idle_timeout"),
        "{}",
        w[0].message
    );
    assert_eq!(
        w[0].fix,
        "write it nested: `serve:` then `  idle_timeout: 5` on the next line"
    );
    let idle = e
        .origins
        .iter()
        .find(|o| o.key == "serve.idle_timeout")
        .unwrap();
    assert_eq!(idle.level, Level::Default, "the dotted key is not read");
}

/// Founder ruling (config typos warn): a dotted review key is a warning; `rdy config` and
/// `rdy status` run and show it.
#[test]
fn msw19_a_dotted_review_key_warns_in_rdy_config_and_rdy_status() {
    let s = Served::new();
    append(
        s.dir.path(),
        PROJECT_FILE,
        "reviews.require_up_to_date: true\n",
    );
    for args in [&["config", "--show-origin"][..], &["status"][..]] {
        let o = s.rdy(args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "{args:?}: {out}");
        assert!(
            out.contains("W_CONFIG_UNKNOWN") && out.contains("is written as one dotted key"),
            "{args:?}: {out}"
        );
        assert!(!out.contains("E_CONFIG"), "{args:?}: {out}");
    }
}

/// id3 (D-ME2C0, D-KQ4CA): the CLI through its daemon, the CLI with `--no-daemon` and MCP refuse
/// an actor or run outside the form with the same error; a human claim has the same code and
/// message in each, with the CLI's fix at the command line and the MCP fix over MCP.
#[test]
fn id3_cli_and_mcp_share_identity_errors() {
    use rdy_qa::mcp::{McpClient, Reply};
    use serde_json::{Value, json};
    let s = Served::new();
    let triple = |v: &Value| {
        (
            v["code"].as_str().unwrap_or_default().to_string(),
            v["message"].as_str().unwrap_or_default().to_string(),
            v["fix"].as_str().unwrap_or_default().to_string(),
        )
    };
    let cli = |actor: &str, run: Option<&str>, no_daemon: bool| {
        let mut args = vec![];
        if no_daemon {
            args.push("--no-daemon");
        }
        args.extend(["--json", "status"]);
        let (ok, v) = s.json_as(actor, run, &args);
        assert!(!ok, "{actor:?} {run:?} (no daemon: {no_daemon}): {v}");
        triple(&v)
    };
    let mut mcp = McpClient::start_proxy(&s);
    let mut over_mcp = |actor: &str, run: Option<&str>| {
        let mut args = json!({"actor": actor, "role": "writer"});
        if let Some(r) = run {
            args["run"] = json!(r);
        }
        match mcp.call("rdy_session", args) {
            Reply::ToolError(v) => triple(&v),
            other => panic!("{actor:?} {run:?}: expected a tool error, got {other:?}"),
        }
    };
    let long_actor = format!("agent:{}", "x".repeat(200));
    for (actor, run) in [
        ("Agent:X", None),
        ("agent:x ", Some("r1")),
        (long_actor.as_str(), None),
        ("agent:x", Some("run:7")),
        ("agent:x", Some("work")),
        ("agent:x", Some(".hidden")),
    ] {
        let daemon = cli(actor, run, false);
        assert_eq!(daemon.0, "E_BAD_INPUT", "{actor:?} {run:?}: {daemon:?}");
        assert!(
            daemon.1.ends_with(" is not in rdy's actor form")
                || daemon.1.ends_with(" is not in rdy's run form"),
            "{daemon:?}"
        );
        assert_eq!(
            cli(actor, run, true),
            daemon,
            "{actor:?} {run:?}: --no-daemon"
        );
        assert_eq!(over_mcp(actor, run), daemon, "{actor:?} {run:?}: MCP");
    }
    // a human claim: one code and message; each transport gives its own fix
    for (actor, run) in [("human:other", None), ("human:other", Some("r1"))] {
        let message = "an agent session cannot claim a human actor".to_string();
        let cli_fix = "unset RDY_ACTOR to act as the person, or relay their words".to_string();
        let want = ("E_PERMISSION".to_string(), message.clone(), cli_fix);
        assert_eq!(cli(actor, run, false), want, "{actor:?} {run:?}");
        assert_eq!(
            cli(actor, run, true),
            want,
            "{actor:?} {run:?}: --no-daemon"
        );
        assert_eq!(
            over_mcp(actor, run),
            (
                "E_PERMISSION".to_string(),
                message,
                "claim your own agent: actor, and relay the person's words with rdy_answer"
                    .to_string()
            ),
            "{actor:?} {run:?}: MCP"
        );
    }
    // MCP is an agent surface: even the configured human's own actor is a claim there
    assert_eq!(over_mcp("human:qa", None).0, "E_PERMISSION");
    // the refusals above came while the daemon held the store: `--no-daemon` checked the
    // identity before it opened the store. The configured human at the command line is the
    // human, both ways (the store is free once the daemon stops).
    let (ok, v) = s.json_as("human:qa", Some("r1"), &["--json", "status"]);
    assert!(ok, "{v}");
    drop(mcp);
    s.stop();
    let (ok, v) = s.json_as("human:qa", Some("r1"), &["--no-daemon", "--json", "status"]);
    assert!(ok, "{v}");
}

/// id3 (D-ME2C0): an `RDY_ACTOR` or `RDY_RUN` that is not Unicode is a value outside the form,
/// never an unset variable. Through the daemon, with `--no-daemon`, and in the session of a
/// reused profile site, the CLI refuses it with the grammar's bounded `E_BAD_INPUT` before it
/// classifies the session: it neither becomes the configured human nor loses its run. The
/// refused commands start no daemon.
#[test]
fn id3_a_non_unicode_actor_or_run_is_refused_on_every_cli_path() {
    use rdy_qa::platform::not_unicode;
    use serde_json::Value;
    use std::ffi::OsStr;
    let s = Served::new();
    let triple = |v: &Value| {
        (
            v["code"].as_str().unwrap_or_default().to_string(),
            v["message"].as_str().unwrap_or_default().to_string(),
            v["fix"].as_str().unwrap_or_default().to_string(),
        )
    };
    let want = |e: rdy_engine::RdyError| (e.code.to_string(), e.message, e.fix);
    let bad_actor = want(rdy_engine::identity::validate_actor("agent:\u{fffd}").unwrap_err());
    let bad_human = want(rdy_engine::identity::validate_actor("human:qa\u{fffd}").unwrap_err());
    let bad_run = want(rdy_engine::identity::validate_run("r\u{fffd}").unwrap_err());
    let agent_actor = not_unicode("agent:");
    let human_actor = not_unicode("human:qa");
    let run = not_unicode("r");
    // an environment actor, its run, and the refusal they get
    type Case<'a> = (&'a OsStr, Option<&'a OsStr>, &'a (String, String, String));
    let cases: [Case<'_>; 5] = [
        (&agent_actor, None, &bad_actor),
        (&human_actor, None, &bad_human),
        (&agent_actor, Some(OsStr::new("r1")), &bad_actor),
        (OsStr::new("agent:x"), Some(&run), &bad_run),
        (OsStr::new("human:qa"), Some(&run), &bad_run),
    ];
    for (actor, run, want) in cases {
        for args in [
            &["--no-daemon", "--json", "status"][..],
            &["--json", "status"][..],
        ] {
            let (ok, v) = s.json_as_os(actor, run, args);
            assert!(!ok, "{actor:?} {run:?} {args:?}: {v}");
            assert_eq!(&triple(&v), want, "{actor:?} {run:?} {args:?}");
        }
    }
    assert!(
        s.daemon_pid().is_none(),
        "a refused command starts no daemon"
    );
    // a running daemon's profile site: the session handshake of `serve --profile`
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let pid = s.daemon_pid().expect("the daemon runs");
    for (actor, run, want) in cases {
        let (ok, v) = s.json_as_os(actor, run, &["--json", "serve", "--profile", "guide"]);
        assert!(!ok, "{actor:?} {run:?}: {v}");
        assert_eq!(&triple(&v), want, "{actor:?} {run:?}: serve --profile");
    }
    assert_eq!(s.daemon_pid(), Some(pid), "the same daemon runs");
    s.stop();
}

/// id3 (D-Q8NFG, fix round 2): a configured actor outside the form is the plain `E_BAD_INPUT`
/// naming its file on every CLI path, also when no daemon runs. The CLI resolves the configured
/// person before it starts a daemon, so the refusal is never the generic `E_DAEMON_START`, and
/// the refused command starts none. A local actor and a user actor are each named by their
/// own file; a running daemon gives the same refusal once the configured actor goes bad.
#[test]
fn id3_an_invalid_configured_actor_is_refused_before_a_daemon_starts() {
    use serde_json::Value;
    let s = Served::new();
    let triple = |v: &Value| {
        (
            v["code"].as_str().unwrap_or_default().to_string(),
            v["message"].as_str().unwrap_or_default().to_string(),
            v["fix"].as_str().unwrap_or_default().to_string(),
        )
    };
    let forms = "human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
    let want = |file: &str| {
        (
            "E_BAD_INPUT".to_string(),
            "the actor \"Nick\" is not in rdy's actor form".to_string(),
            format!("set actor in {file} to {forms}"),
        )
    };
    let refused_everywhere = |want: &(String, String, String), what: &str| {
        for args in [
            &["--json", "status"][..],
            &["--no-daemon", "--json", "status"][..],
        ] {
            let (ok, v) = s.json(args);
            assert!(!ok, "{what} {args:?}: {v}");
            assert_eq!(&triple(&v), want, "{what} {args:?} as the person");
            let (ok, v) = s.json_as("agent:w", Some("r1"), args);
            assert!(!ok, "{what} {args:?}: {v}");
            assert_eq!(&triple(&v), want, "{what} {args:?} as an agent");
        }
    };
    let local = s.dir.path().join(LOCAL_FILE);
    let local_valid = std::fs::read_to_string(&local).ok();
    let restore_local = || match &local_valid {
        Some(t) => std::fs::write(&local, t).unwrap(),
        None => std::fs::remove_file(&local).unwrap(),
    };
    let user_file = s.user_config();
    let user_valid = std::fs::read_to_string(&user_file).unwrap();
    // a user actor outside the form, no daemon running
    std::fs::write(&user_file, "actor: Nick\n").unwrap();
    refused_everywhere(&want(&user_file.display().to_string()), "user");
    assert!(
        s.daemon_pid().is_none(),
        "a refused command starts no daemon"
    );
    // a local actor outside the form, no daemon running
    std::fs::write(&user_file, &user_valid).unwrap();
    append(s.dir.path(), LOCAL_FILE, "actor: Nick\n");
    refused_everywhere(&want(LOCAL_FILE), "local");
    assert!(
        s.daemon_pid().is_none(),
        "a refused command starts no daemon"
    );
    // a running daemon: the same refusal once the local actor goes bad
    restore_local();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let pid = s.daemon_pid().expect("the daemon runs");
    append(s.dir.path(), LOCAL_FILE, "actor: Nick\n");
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(!ok, "{v}");
    assert_eq!(triple(&v), want(LOCAL_FILE), "a running daemon");
    assert_eq!(s.daemon_pid(), Some(pid), "the same daemon runs");
    restore_local();
    s.stop();
}

/// id3 (D-Q8NFG, final ruling): a configured actor outside the form is the same `E_BAD_INPUT`
/// naming its file on every cold-start transport as on an ordinary command: `rdy serve`
/// (foreground and detached), `rdy mcp` through the daemon proxy and `rdy mcp --no-daemon`.
/// Each exits as `rdy status` does, prints the identical code, message and fix, and leaves no
/// `.rdy/run` and no daemon: the CLI resolves the configured person before any transport starts.
#[test]
fn id3_cold_start_transports_refuse_an_invalid_configured_actor_as_status_does() {
    use serde_json::Value;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let s = Served::new();
    append(s.dir.path(), LOCAL_FILE, "actor: Nick\n");
    let run = s.run_dir();
    assert!(!run.path().exists(), "a fresh project has no .rdy/run");
    // runs the command with no stdin (an MCP session that never gets a request), killed past a
    // hang guard so a transport that starts serving fails the test instead of hanging it
    let output = |args: &[&str]| {
        let mut child = rdy_qa::daemon::human_command(s.dir.path(), &s.env_pairs(), args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rdy");
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().expect("wait for rdy").is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().expect("collect rdy's output")
    };
    let parsed = |out: &std::process::Output| -> Value {
        let text = String::from_utf8_lossy(&out.stderr);
        serde_json::from_str(text.trim().lines().last().unwrap_or(""))
            .unwrap_or_else(|_| Value::String(text.into_owned()))
    };
    let status = output(&["--json", "status"]);
    let want = parsed(&status);
    let forms = "human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
    assert_eq!(want["code"], "E_BAD_INPUT", "{want}");
    assert_eq!(
        want["message"], "the actor \"Nick\" is not in rdy's actor form",
        "{want}"
    );
    assert_eq!(
        want["fix"],
        format!("set actor in {LOCAL_FILE} to {forms}"),
        "{want}"
    );
    for args in [
        &["--json", "serve"][..],
        &["--json", "serve", "--detach"][..],
        &["--json", "mcp"][..],
        &["--json", "--no-daemon", "mcp"][..],
    ] {
        let out = output(args);
        assert_eq!(out.status.code(), status.status.code(), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        assert_eq!(
            parsed(&out),
            want,
            "{args:?}: the same refusal as rdy status"
        );
        assert!(!run.path().exists(), "{args:?} leaves no .rdy/run");
        assert!(s.daemon_pid().is_none(), "{args:?} starts no daemon");
    }
}

/// The JSON rows and the text lines (settings only, no warnings) of `rdy config`.
fn relc5_listing(s: &Served, show_origin: bool) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut json = vec!["--json", "config"];
    let mut text = vec!["config"];
    if show_origin {
        json.push("--show-origin");
        text.push("--show-origin");
    }
    let (okk, v) = s.json(&json);
    assert!(okk, "{v}");
    let out = String::from_utf8_lossy(&s.rdy(&text).stdout).into_owned();
    let lines = out
        .lines()
        .filter(|l| !l.starts_with("warning "))
        .map(str::to_string)
        .collect();
    (v.as_array().unwrap().clone(), lines)
}

/// The table slot of a listed key: a plain key is its own slot, an entry row belongs to its group.
fn relc5_slot(key: &str) -> usize {
    KEY_LEVELS
        .iter()
        .position(|r| {
            r.key == key
                || (["code_paths", "remotes", "hub.installs"].contains(&r.key)
                    && key.starts_with(&format!("{}.", r.key)))
        })
        .unwrap_or_else(|| panic!("{key} is in no slot"))
}

fn relc5_levels(slot: usize) -> Vec<String> {
    KEY_LEVELS[slot]
        .levels
        .iter()
        .map(|l| {
            serde_json::to_value(l)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// Checks the rules every listing keeps, whatever the values: each table slot is listed, in
/// table order, each row names its slot's allowed levels in declared order, and the text lines
/// list the JSON rows' keys in the same order, each ending in the allowed levels.
fn relc5_check(rows: &[serde_json::Value], lines: &[String], show_origin: bool) {
    let slots: Vec<usize> = rows
        .iter()
        .map(|r| relc5_slot(r["key"].as_str().unwrap()))
        .collect();
    assert!(slots.windows(2).all(|w| w[0] <= w[1]), "{slots:?}");
    for i in 0..KEY_LEVELS.len() {
        assert!(slots.contains(&i), "{} is not listed", KEY_LEVELS[i].key);
    }
    assert_eq!(rows.len(), lines.len(), "{rows:?}\n{lines:?}");
    for (r, l) in rows.iter().zip(lines) {
        let key = r["key"].as_str().unwrap();
        let want: Vec<String> = relc5_levels(relc5_slot(key));
        let got: Vec<String> = r["levels"]
            .as_array()
            .unwrap_or_else(|| panic!("{r}"))
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "{r}");
        assert!(l.starts_with(&format!("{key} = ")), "{l}");
        assert!(
            l.ends_with(&format!("  # set at: {}", want.join(", "))),
            "{l}"
        );
        if r["level"].is_null() {
            assert!(r["value"].is_null() && r["file"].is_null(), "{r}");
            let tail = if show_origin { "  # unset" } else { "" };
            assert!(
                l.starts_with(&format!("{key} = unset{tail}  # set at: ")),
                "{l}"
            );
        }
    }
}

fn relc5_row<'a>(rows: &'a [serde_json::Value], key: &str) -> &'a serde_json::Value {
    rows.iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no row {key}: {rows:?}"))
}

#[test]
fn relc5_config_lists_every_key_and_allowed_levels() {
    use rdy_qa::git_repo::*;
    let s = Served::new();
    // nothing configured: every key is listed; one unset row per empty group
    for show_origin in [false, true] {
        let (rows, lines) = relc5_listing(&s, show_origin);
        relc5_check(&rows, &lines, show_origin);
        for key in [
            "code_paths",
            "remotes",
            "hub.installs",
            "signing.key",
            "signing.agent_key",
            "signing.allowed_signers",
        ] {
            let r = relc5_row(&rows, key);
            assert!(
                r["value"].is_null() && r["level"].is_null() && r["file"].is_null(),
                "{r}"
            );
        }
        let d = relc5_row(&rows, "serve.start_timeout");
        assert_eq!(
            (d["value"].clone(), d["level"].clone()),
            (30.into(), "default".into())
        );
        assert!(d["file"].is_null(), "{d}");
        assert_eq!(relc5_row(&rows, "pack")["level"], "project");
        assert_eq!(relc5_row(&rows, "actor")["level"], "user");
        let line = |k: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(&format!("{k} = ")))
                .unwrap()
                .clone()
        };
        if show_origin {
            assert_eq!(
                line("signing.key"),
                "signing.key = unset  # unset  # set at: local, user"
            );
            assert_eq!(
                line("serve.start_timeout"),
                "serve.start_timeout = 30  # default  # set at: local, project, user"
            );
        } else {
            assert_eq!(
                line("code_paths"),
                "code_paths = unset  # set at: local, user"
            );
        }
    }

    // values at every level, two code repos with different winning levels, an explicit null
    let app = GitRepo::at(&s.dir.path().join("checkouts").join("app"));
    app.write("a.txt", "a\n");
    app.commit("feat: a");
    let lib = GitRepo::at(&s.dir.path().join("lib"));
    lib.write("b.txt", "b\n");
    lib.commit("feat: b");
    append(
        s.dir.path(),
        PROJECT_FILE,
        "code_repos: [app, lib]\nname:\npropose:\n  changes: 7\n",
    );
    append(
        s.dir.path(),
        LOCAL_FILE,
        "code_paths:\n  app: checkouts/app\nsigning:\n  sign: always\n",
    );
    let uf = s.user_config();
    let mut text = std::fs::read_to_string(&uf).unwrap();
    text.push_str(&format!(
        "views:\n  idle_days: 4\ncode_paths:\n  lib: {}\n  app: elsewhere\n",
        serde_json::to_string(&lib.path().to_string_lossy()).unwrap()
    ));
    std::fs::write(&uf, text).unwrap();
    let local_file = s.dir.path().join(".rdy").join("local.yaml");
    for show_origin in [false, true] {
        let (rows, lines) = relc5_listing(&s, show_origin);
        relc5_check(&rows, &lines, show_origin);
        let at = |k: &str| {
            (
                relc5_row(&rows, k)["level"].clone(),
                relc5_row(&rows, k)["value"].clone(),
            )
        };
        assert_eq!(at("propose.changes"), ("project".into(), 7.into()));
        assert_eq!(at("views.idle_days"), ("user".into(), 4.into()));
        assert_eq!(at("signing.sign"), ("local".into(), "always".into()));
        let n = relc5_row(&rows, "name");
        assert!(n["value"].is_null() && n["level"] == "project", "{n}");
        assert!(n["file"].as_str().unwrap().ends_with(".rdy.yaml"), "{n}");
        assert_eq!(
            lines
                .iter()
                .find(|l| l.starts_with("name = "))
                .unwrap()
                .split("  #")
                .next(),
            Some("name = null")
        );
        let paths: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|r| r["key"].as_str().unwrap().starts_with("code_paths"))
            .collect();
        assert_eq!(
            paths
                .iter()
                .map(|r| r["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["code_paths.app", "code_paths.lib"],
            "no aggregate row beside the repo rows"
        );
        assert_eq!(
            (paths[0]["level"].clone(), paths[0]["file"].clone()),
            ("local".into(), serde_json::json!(local_file))
        );
        assert_eq!(
            (paths[1]["level"].clone(), paths[1]["file"].clone()),
            ("user".into(), serde_json::json!(uf))
        );
    }
}
