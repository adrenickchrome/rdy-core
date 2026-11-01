//! ui4 (D-6M0AF, D-94BT2; system/configuration p-P16JT, reference/cli tb-JYKPD): `serve.ui`
//! turns the local approval pages on or off. It is a boolean of the user and local configs only,
//! the local value wins, and an absent value is `true`. In `.rdy.yaml` it is a `W_CONFIG_UNKNOWN`
//! warning and rdy ignores it. The key table enforces the levels and the type, with the existing
//! texts. `site.base_path` is the existing project key and keeps its behaviour.
use rdy_engine::config_levels::{
    Effective, GivenFiles, KEY_LEVELS, LOCAL_FILE, Level, PROJECT_FILE, Ty, effective,
    effective_from, effective_lenient, valid_site_base_path,
};
use rdy_engine::{E_CONFIG, Rdy, ServeConfig, serve_config};
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

const KEY: &str = "serve.ui";

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

/// `serve.ui` as the engine reads it: the effective settings with `user`, the lenient client
/// read, and an open project (the last two read the environment's user config, which a local
/// value overrides).
fn ui_of(p: &Path, user: Option<&Path>) -> (Value, Level) {
    origin(&effective(p, user).unwrap(), KEY)
}

#[test]
fn ui4_ui_defaults_true_and_only_user_local_override() {
    // the key table: one row after `serve.site_lan_address`, user and local levels, a boolean
    // whose default is true
    let keys: Vec<&str> = KEY_LEVELS.iter().map(|r| r.key).collect();
    let at = keys
        .iter()
        .position(|k| *k == KEY)
        .expect("serve.ui is a key of the table");
    assert_eq!(keys[at - 1], "serve.site_lan_address");
    assert_eq!(keys.iter().filter(|k| **k == KEY).count(), 1);
    let rule = &KEY_LEVELS[at];
    assert_eq!(rule.levels, &[Level::Local, Level::User]);
    assert_eq!(rule.ty, Ty::Bool);
    assert_eq!(rule.default, Some("true"));
    assert!(ServeConfig::DEFAULT.ui, "the default is on");
    // no MCP-over-HTTP key comes with it
    assert!(!keys.iter().any(|k| k.starts_with("serve.mcp")), "{keys:?}");

    // absent: true, from the default level, no warning
    let (_d, p) = project();
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(true), Level::Default));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert!(!e.origins.iter().any(|o| o.key.starts_with("serve.mcp")));
    assert!(serve_config(&p).ui);
    assert!(Rdy::open(&p).unwrap().serve_config().ui);

    // both values in the local config
    for v in [false, true] {
        local(&p, Some(&format!("serve:\n  ui: {v}\n")));
        assert_eq!(ui_of(&p, None), (Value::Bool(v), Level::Local), "{v}");
        assert_eq!(serve_config(&p).ui, v, "{v}");
        assert_eq!(Rdy::open(&p).unwrap().serve_config().ui, v, "{v}");
        let e = effective(&p, None).unwrap();
        assert!(e.warnings.is_empty(), "{v}: {:?}", e.warnings);
    }

    // both values in the user config; the local value wins over it
    local(&p, None);
    for v in [false, true] {
        let (_u, uf) = user(&format!("serve:\n  ui: {v}\n"));
        assert_eq!(ui_of(&p, Some(&uf)), (Value::Bool(v), Level::User), "{v}");
        local(&p, Some(&format!("serve:\n  ui: {}\n", !v)));
        assert_eq!(
            ui_of(&p, Some(&uf)),
            (Value::Bool(!v), Level::Local),
            "local over user {v}"
        );
        assert_eq!(serve_config(&p).ui, !v);
        local(&p, None);
    }

    // in the project config: a warning naming the right files, and rdy ignores it
    let (_u, uf) = user("serve:\n  ui: false\n");
    let shown = uf.display().to_string();
    project_file(&p, "serve:\n  ui: false\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(true), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        e.warnings[0].message,
        "serve.ui in .rdy.yaml belongs in .rdy/local.yaml or the user config; rdy ignores it here"
    );
    assert_eq!(
        e.warnings[0].fix,
        "move serve.ui to .rdy/local.yaml or the user config"
    );
    assert!(serve_config(&p).ui, "the project value is ignored");
    assert!(Rdy::open(&p).unwrap().serve_config().ui);
    let e = effective(&p, Some(&uf)).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(false), Level::User));
    assert_eq!(
        e.warnings[0].message,
        format!(
            "serve.ui in .rdy.yaml belongs in .rdy/local.yaml or the user config ({shown}); rdy ignores it here"
        )
    );
    assert_eq!(
        e.warnings[0].fix,
        format!("move serve.ui to .rdy/local.yaml or the user config ({shown})")
    );
    // a wrong type there is the same warning, never a refusal
    project_file(&p, "serve:\n  ui: sometimes\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(true), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0]
            .message
            .starts_with("serve.ui in .rdy.yaml belongs in")
    );
    project_file(&p, "");

    // a wrong type in a file that allows the key: the existing boolean refusal; the lenient
    // client read keeps the default
    for bad in ["sometimes", "1", "[true]", "\"true\""] {
        local(&p, Some(&format!("serve:\n  ui: {bad}\n")));
        let err = effective(&p, None).unwrap_err();
        assert_eq!(err.code, E_CONFIG, "{bad}");
        assert!(
            err.message
                .starts_with("serve.ui in .rdy/local.yaml: invalid type"),
            "{bad}: {}",
            err.message
        );
        assert_eq!(err.fix, "fix serve.ui in .rdy/local.yaml", "{bad}");
        assert!(serve_config(&p).ui, "{bad}");
        let lenient = effective_lenient(&p, None);
        assert_eq!(
            origin(&lenient, KEY),
            (Value::Bool(true), Level::Default),
            "{bad}"
        );
    }
    local(&p, None);
    let (_u, bad_user) = user("serve:\n  ui: sometimes\n");
    let err = effective(&p, Some(&bad_user)).unwrap_err();
    assert_eq!(err.code, E_CONFIG);
    assert_eq!(
        err.message.split(": ").next(),
        Some(format!("serve.ui in {}", bad_user.display()).as_str())
    );
    assert_eq!(err.fix, format!("fix serve.ui in {}", bad_user.display()));

    // a close key: the nearest-key hint of the table
    local(&p, Some("serve:\n  uii: false\n"));
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, KEY), (Value::Bool(true), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        e.warnings[0].message,
        "unknown key serve.uii in .rdy/local.yaml (local level); rdy ignores it"
    );
    assert_eq!(
        e.warnings[0].fix,
        "did you mean serve.ui? rename serve.uii in .rdy/local.yaml"
    );
    local(&p, None);
    project_file(&p, "serve:\n  uii: false\n");
    let e = effective(&p, None).unwrap();
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(
        e.warnings[0].message,
        "unknown key serve.uii in .rdy.yaml (project level); rdy ignores it"
    );
    assert_eq!(
        e.warnings[0].fix,
        "did you mean serve.ui? set it in .rdy/local.yaml or the user config and remove serve.uii from .rdy.yaml"
    );
    // an MCP-over-HTTP key is unknown too: no such setting exists
    project_file(&p, "");
    local(&p, Some("serve:\n  mcp_http: true\n"));
    let e = effective(&p, None).unwrap();
    assert!(!e.origins.iter().any(|o| o.key == "serve.mcp_http"));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0]
            .message
            .starts_with("unknown key serve.mcp_http in .rdy/local.yaml"),
        "{}",
        e.warnings[0].message
    );
}

/// A project file holding `pack` and the given `site.base_path` string.
fn base_path_yaml(base_path: &str) -> String {
    let mut site = Mapping::new();
    site.insert("base_path".into(), Value::String(base_path.to_string()));
    let mut top = Mapping::new();
    top.insert("pack".into(), "rdy-standard".into());
    top.insert("site".into(), Value::Mapping(site));
    serde_yaml_ng::to_string(&Value::Mapping(top)).unwrap()
}

fn base_path_of(yaml: &str) -> Effective {
    let d = tempfile::tempdir().unwrap();
    effective_from(
        d.path(),
        None,
        GivenFiles {
            project: yaml.as_bytes(),
            local: None,
        },
    )
    .unwrap()
}

/// Pins existing behaviour: `serve.ui` adds no base-path key and changes none of its texts.
#[test]
fn ui4_site_base_path_is_existing_and_unchanged() {
    const BP: &str = "site.base_path";
    const DETAIL: &str = "site.base_path must be / or an absolute path that ends with /, at most 2048 bytes, with only letters, digits, -, _, . and ~ between the slashes; using /";
    const FIX: &str = "set site.base_path to / or to an absolute path that ends with /, such as /docs/, in .rdy.yaml";
    let rule = KEY_LEVELS.iter().find(|r| r.key == BP).expect("the key");
    assert_eq!(rule.levels, &[Level::Project]);
    assert_eq!(rule.ty, Ty::BasePath);
    assert_eq!(rule.default, Some("/"));
    assert_eq!(KEY_LEVELS.iter().filter(|r| r.key == BP).count(), 1);

    let long = |n: usize| format!("/{}/", "a".repeat(n - 2));
    // root and /docs/ and the longest value are kept, with no warning
    for ok in ["/".to_string(), "/docs/".to_string(), long(2048)] {
        assert!(valid_site_base_path(&ok), "{ok}");
        let e = base_path_of(&base_path_yaml(&ok));
        assert_eq!(origin(&e, BP), (Value::from(ok.as_str()), Level::Project));
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    }
    // one byte past and a malformed path warn with the closed texts and read as `/`
    for bad in [long(2049), "docs".into(), "/a//b/".into(), "/a%20b/".into()] {
        assert!(!valid_site_base_path(&bad), "{bad}");
        let e = base_path_of(&base_path_yaml(&bad));
        assert_eq!(e.warnings.len(), 1, "{bad}: {:?}", e.warnings);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert_eq!(e.warnings[0].message, DETAIL, "{bad}");
        assert_eq!(e.warnings[0].fix, FIX, "{bad}");
        assert_eq!(origin(&e, BP).0, Value::from("/"), "{bad}");
    }
    // project level only: a local value warns and rdy ignores it
    let (_d, p) = project();
    local(&p, Some("site:\n  base_path: /local/\n"));
    let e = effective(&p, None).unwrap();
    assert_eq!(origin(&e, BP), (Value::from("/"), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(
        e.warnings[0].message,
        "site.base_path in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here"
    );
    assert_eq!(e.warnings[0].fix, "move site.base_path to .rdy.yaml");
    local(&p, None);

    // the live site ignores it: the static build's not-found page is rooted at the base path,
    // and a live build writes no not-found file
    std::fs::write(p.join(PROJECT_FILE), base_path_yaml("/docs/")).unwrap();
    let r = Rdy::open(&p).unwrap();
    let read = r.read();
    assert_eq!(read.site_base_path(), "/docs/");
    let live = rdy_render::build_site(
        &read,
        &rdy_engine::Scope::Canon,
        rdy_render::Mode::Live { root: "/" },
    )
    .unwrap();
    assert!(
        !live.files.keys().any(|k| k == "404.html"),
        "a live build has no static not-found page"
    );
    let built =
        rdy_render::build_site(&read, &rdy_engine::Scope::Canon, rdy_render::Mode::Static).unwrap();
    let page = String::from_utf8(built.files["404.html"].clone()).unwrap();
    assert!(page.contains("href=\"/docs/index.html\""), "{page}");
}
