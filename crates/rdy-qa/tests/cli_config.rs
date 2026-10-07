//! cfgl1: settings come from four levels (FW-X5, D-C5955) minus the file move: .rdy/local.yaml,
//! the project config (.rdy/config.yaml until M2f-b), the user config and the defaults; the most
//! specific wins; a key at the wrong level is E_CONFIG naming the right one; rdy config
//! --show-origin prints each value's file.
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
    assert_eq!(human_actor_for(&p, Some(&uf), Some("u")), "human:here");
    let fp = pack_fingerprint(&p).unwrap();
    assert!(
        fp.files.iter().any(|(f, _, _)| f.ends_with(LOCAL_FILE)),
        "a local change reloads like a config change"
    );
}

#[test]
fn cfgl1_a_key_at_the_wrong_level_is_e_config_naming_the_level() {
    let (_d, p) = proj();
    append(&p, PROJECT_FILE, "actor: human:all\n");
    let e = effective(&p, None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message.contains("actor")
            && e.fix.contains(".rdy/local.yaml")
            && e.fix.contains("user config"),
        "{e}"
    );
    assert_eq!(
        Rdy::open(&p).err().map(|e| e.code),
        Some(E_CONFIG),
        "open refuses it too"
    );
    let (_d, p) = proj();
    append(&p, LOCAL_FILE, "pack: other\n");
    let e = effective(&p, None).unwrap_err();
    assert!(
        e.message.contains("pack") && e.fix.contains("project config"),
        "{e}"
    );
    let (_d, p) = proj();
    let (_u, uf) = user("decisions:\n  home: x\n");
    let e = effective(&p, Some(&uf)).unwrap_err();
    assert!(e.message.contains("decisions.home"), "{e}");
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
    assert!(row["file"].as_str().unwrap().ends_with(".rdy/local.yaml"));
    let actor = rows.iter().find(|r| r["key"] == "actor").unwrap();
    assert_eq!(actor["value"], "human:qa");
    assert_eq!(actor["level"], "user");
    let text = String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l.starts_with("views.idle_days = 9  # ") && l.ends_with(".rdy/local.yaml")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == "serve.start_timeout = 30  # default"),
        "{text}"
    );
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
        e.fix.contains(".rdy/local.yaml") && !e.fix.contains("config.yaml"),
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
