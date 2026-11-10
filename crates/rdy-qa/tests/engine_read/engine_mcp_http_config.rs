//! mht1 (D-44NVC, D-M919G): `serve.mcp_http` turns MCP over HTTP on or off. It is a boolean of the
//! user and local configs only, the local value wins, and an absent value is `false`. In
//! `.rdy.yaml` it is a `W_CONFIG_UNKNOWN` warning and rdy ignores it. The key table enforces the
//! levels and the type, with the existing texts. The setting adds no listener.
use rdy_engine::config_levels::{
    Effective, KEY_LEVELS, LOCAL_FILE, Level, PROJECT_FILE, Ty, effective, effective_lenient,
};
use rdy_engine::{E_CONFIG, Rdy, ServeConfig, serve_config};
use serde_yaml_ng::Value;
use std::path::{Path, PathBuf};

const KEY: &str = "serve.mcp_http";

/// A new project, closed.
fn project() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let p = d.path().to_path_buf();
    (d, p)
}

/// A user config holding `text`.
fn user(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    std::fs::write(&f, text).unwrap();
    (d, f)
}

/// Replaces `.rdy/local.yaml` (`None` removes it).
fn local(p: &Path, text: Option<&str>) {
    let f = p.join(LOCAL_FILE);
    match text {
        Some(t) => std::fs::write(f, t).unwrap(),
        None => drop(std::fs::remove_file(f)),
    }
}

/// Replaces `.rdy.yaml` with `pack: rdy-standard` and `extra`.
fn project_file(p: &Path, extra: &str) {
    std::fs::write(p.join(PROJECT_FILE), format!("pack: rdy-standard\n{extra}")).unwrap();
}

/// The winning value and level of `key`.
fn origin(e: &Effective, key: &str) -> (Value, Level) {
    let o = e
        .origins
        .iter()
        .find(|o| o.key == key)
        .unwrap_or_else(|| panic!("no origin for {key}"));
    (o.value.clone(), o.level)
}

fn flag_of(p: &Path, user: Option<&Path>) -> (Value, Level) {
    origin(&effective(p, user).unwrap(), KEY)
}

#[test]
fn mht1_http_defaults_off_and_uses_user_local_precedence() {
    // the key table: one row right after `serve.ui`, user and local levels, a boolean, off
    let keys: Vec<&str> = KEY_LEVELS.iter().map(|r| r.key).collect();
    let at = keys
        .iter()
        .position(|k| *k == KEY)
        .expect("serve.mcp_http is a key of the table");
    assert_eq!(keys[at - 1], "serve.ui");
    assert_eq!(keys.iter().filter(|k| **k == KEY).count(), 1);
    let rule = &KEY_LEVELS[at];
    assert_eq!(rule.levels, &[Level::Local, Level::User]);
    assert_eq!(rule.ty, Ty::Bool);
    assert_eq!(rule.default, Some("false"));
    assert!(
        !std::hint::black_box(ServeConfig::DEFAULT).mcp_http,
        "the default is off"
    );

    // absent: false, from the default level, no warning
    let (_d, p) = project();
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(false), Level::Default));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert!(!serve_config(&p).mcp_http);
    assert!(!Rdy::open(&p).unwrap().serve_config().mcp_http);

    // both values in the local config
    for v in [false, true] {
        local(&p, Some(&format!("serve:\n  mcp_http: {v}\n")));
        assert_eq!(flag_of(&p, None), (Value::Bool(v), Level::Local), "{v}");
        assert_eq!(serve_config(&p).mcp_http, v, "{v}");
        assert_eq!(Rdy::open(&p).unwrap().serve_config().mcp_http, v, "{v}");
        let e = effective(&p, None).unwrap();
        assert!(e.warnings.is_empty(), "{v}: {:?}", e.warnings);
    }

    // both values in the user config; the local value wins over it
    local(&p, None);
    for v in [false, true] {
        let (_u, uf) = user(&format!("serve:\n  mcp_http: {v}\n"));
        assert_eq!(flag_of(&p, Some(&uf)), (Value::Bool(v), Level::User), "{v}");
        local(&p, Some(&format!("serve:\n  mcp_http: {}\n", !v)));
        assert_eq!(
            flag_of(&p, Some(&uf)),
            (Value::Bool(!v), Level::Local),
            "local over user {v}"
        );
        assert_eq!(serve_config(&p).mcp_http, !v);
        local(&p, None);
    }
    // the setting is independent of serve.ui
    local(&p, Some("serve:\n  mcp_http: true\n"));
    assert!(serve_config(&p).ui);
    local(&p, Some("serve:\n  ui: false\n"));
    assert!(!serve_config(&p).mcp_http);
}

#[test]
fn mht1_project_enablement_warns_and_is_ignored() {
    let (_d, p) = project();
    let (_u, uf) = user("serve:\n  mcp_http: true\n");
    let shown = uf.display().to_string();
    project_file(&p, "serve:\n  mcp_http: true\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(false), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        e.warnings[0].message,
        "serve.mcp_http in .rdy.yaml belongs in .rdy/local.yaml or the user config; rdy ignores it here"
    );
    assert_eq!(
        e.warnings[0].fix,
        "move serve.mcp_http to .rdy/local.yaml or the user config"
    );
    assert!(!serve_config(&p).mcp_http, "the project value is ignored");
    assert!(!Rdy::open(&p).unwrap().serve_config().mcp_http);
    // the user value still wins, and the warning names the user file
    let e = effective(&p, Some(&uf)).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(true), Level::User));
    assert_eq!(
        e.warnings[0].message,
        format!(
            "serve.mcp_http in .rdy.yaml belongs in .rdy/local.yaml or the user config ({shown}); rdy ignores it here"
        )
    );
    assert_eq!(
        e.warnings[0].fix,
        format!("move serve.mcp_http to .rdy/local.yaml or the user config ({shown})")
    );
    // a wrong type there is the same warning, never a refusal
    project_file(&p, "serve:\n  mcp_http: sometimes\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(false), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0]
            .message
            .starts_with("serve.mcp_http in .rdy.yaml belongs in")
    );
}

#[test]
fn mht1_invalid_type_and_near_key_use_config_errors() {
    let (_d, p) = project();
    // a wrong type in a file that allows the key: the existing boolean refusal; the lenient
    // client read keeps the default
    for bad in ["sometimes", "1", "[true]", "\"true\""] {
        local(&p, Some(&format!("serve:\n  mcp_http: {bad}\n")));
        let err = effective(&p, None).unwrap_err();
        assert_eq!(err.code, E_CONFIG, "{bad}");
        assert!(
            err.message
                .starts_with("serve.mcp_http in .rdy/local.yaml: invalid type"),
            "{bad}: {}",
            err.message
        );
        assert_eq!(err.fix, "fix serve.mcp_http in .rdy/local.yaml", "{bad}");
        assert!(!serve_config(&p).mcp_http, "{bad}");
        let lenient = effective_lenient(&p, None);
        assert_eq!(
            origin(&lenient, KEY),
            (Value::Bool(false), Level::Default),
            "{bad}"
        );
    }
    local(&p, None);
    let (_u, bad_user) = user("serve:\n  mcp_http: sometimes\n");
    let err = effective(&p, Some(&bad_user)).unwrap_err();
    assert_eq!(err.code, E_CONFIG);
    assert_eq!(
        err.message.split(": ").next(),
        Some(format!("serve.mcp_http in {}", bad_user.display()).as_str())
    );
    assert_eq!(
        err.fix,
        format!("fix serve.mcp_http in {}", bad_user.display())
    );

    // a close key: the nearest-key hint of the table
    local(&p, Some("serve:\n  mcp_htp: true\n"));
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(false), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        e.warnings[0].message,
        "unknown key serve.mcp_htp in .rdy/local.yaml (local level); rdy ignores it"
    );
    assert_eq!(
        e.warnings[0].fix,
        "did you mean serve.mcp_http? rename serve.mcp_htp in .rdy/local.yaml"
    );
    local(&p, None);
    project_file(&p, "serve:\n  mcp_htp: true\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(
        e.warnings[0].message,
        "unknown key serve.mcp_htp in .rdy.yaml (project level); rdy ignores it"
    );
    assert_eq!(
        e.warnings[0].fix,
        "did you mean serve.mcp_http? set it in .rdy/local.yaml or the user config and remove serve.mcp_htp from .rdy.yaml"
    );
}
