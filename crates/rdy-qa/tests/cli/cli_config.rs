//! cfgl1: settings come from four levels (FW-X5, D-C5955) minus the file move: .rdy/local.yaml,
//! the project config (.rdy/config.yaml until M2f-b), the user config and the defaults; the most
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
    assert_eq!(human_actor_for(&p, Some(&uf), Some("u")), "human:here");
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
        e.warnings[0].message.contains("pack") && e.warnings[0].fix.contains(".rdy/config.yaml"),
        "{:?}",
        e.warnings[0]
    );
    assert!(Rdy::open(&p).is_ok(), "open accepts it too");
    let (_d, p) = proj();
    let (_u, uf) = user("decisions:\n  home: x\n");
    let e = effective(&p, Some(&uf)).expect("a wrong-file key is a warning");
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0].message.contains("decisions.home")
            && e.warnings[0].fix.contains(".rdy/config.yaml"),
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
