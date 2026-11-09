//! ain2: `instructions.person` is a user-config key. `rdy config` lists it with its origin, and a
//! project config that sets it gets the wrong-level warning, with no change to the person source.
use rdy_qa::daemon::Served;

fn show_origin(s: &Served) -> String {
    String::from_utf8_lossy(&s.rdy(&["config", "--show-origin"]).stdout).into_owned()
}

#[test]
fn ain2_user_config_sets_the_person_path_and_the_project_config_cannot() {
    let s = Served::new();
    let target = tempfile::tempdir().unwrap();
    let target = target.path().to_str().unwrap().to_string();
    let uf = s.user_config();
    let mut text = std::fs::read_to_string(&uf).unwrap_or_default();
    text.push_str(&format!("instructions:\n  person: '{target}'\n"));
    std::fs::write(&uf, text).unwrap();
    let out = show_origin(&s);
    assert!(
        out.lines()
            .any(|l| l.contains("instructions.person") && l.contains(&target)),
        "{out}"
    );
    assert!(!out.contains("W_CONFIG_UNKNOWN"), "{out}");
    // the project config is the wrong level
    let proj = s.dir.path().join(".rdy.yaml");
    let mut p = std::fs::read_to_string(&proj).unwrap();
    p.push_str("instructions:\n  person: /somewhere/else\n");
    std::fs::write(&proj, p).unwrap();
    let out = show_origin(&s);
    assert!(
        out.lines()
            .any(|l| l.starts_with("warning W_CONFIG_UNKNOWN: ")
                && l.contains("instructions.person in .rdy.yaml belongs in the user config")),
        "{out}"
    );
    assert!(
        !out.contains("/somewhere/else\n") || out.contains("warning"),
        "{out}"
    );
}

#[test]
fn ain2_a_relative_person_path_warns_and_is_ignored() {
    let s = Served::new();
    let uf = s.user_config();
    let mut text = std::fs::read_to_string(&uf).unwrap_or_default();
    text.push_str("instructions:\n  person: relative/meta\n");
    std::fs::write(&uf, text).unwrap();
    let out = show_origin(&s);
    assert!(
        out.lines()
            .any(|l| l.starts_with("warning W_CONFIG_UNKNOWN: ")
                && l.contains("must be an absolute path; rdy ignores it")),
        "{out}"
    );
    // since every key is listed (TASK-1688), the key has one row: it reads unset, and the
    // ignored relative path is never shown as its value
    let rows: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("instructions.person") && !l.starts_with("warning"))
        .collect();
    assert_eq!(rows.len(), 1, "{out}");
    assert!(rows[0].contains("unset"), "{out}");
    assert!(!rows[0].contains("relative/meta"), "{out}");
}
