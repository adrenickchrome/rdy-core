//! dgm5 (D-94BT2, D-E8P8J, D-WQ5GC, D-YXK1V; system/configuration p-92MRG, reference/cli
//! tb-JYKPD): `site.base_path` is a project setting. It defaults to `/`, a value outside the
//! closed form warns and reads as `/`, only the project config sets it, and a published state
//! keeps the value it was published with.
use rdy_engine::config_levels::{
    GivenFiles, KEY_LEVELS, Level, effective, effective_from, valid_site_base_path,
};
use rdy_engine::{E_CONFIG, Rdy, ReloadOutcome, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_yaml_ng::{Mapping, Value};
use std::path::Path;

const KEY: &str = "site.base_path";
const DETAIL: &str = "site.base_path must be / or an absolute path that ends with /, at most 2048 bytes, with only letters, digits, -, _, . and ~ between the slashes; using /";
const FIX: &str =
    "set site.base_path to / or to an absolute path that ends with /, such as /docs/, in .rdy.yaml";

/// A project file holding `pack` and the given `site.base_path` string.
fn project_yaml(base_path: &str) -> String {
    let mut site = Mapping::new();
    site.insert("base_path".into(), Value::String(base_path.to_string()));
    let mut top = Mapping::new();
    top.insert("pack".into(), "rdy-standard".into());
    top.insert("site".into(), Value::Mapping(site));
    serde_yaml_ng::to_string(&Value::Mapping(top)).unwrap()
}

/// The effective settings of a project whose `.rdy.yaml` holds `yaml`, with no user config.
fn effective_of(yaml: &str) -> rdy_engine::config_levels::Effective {
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

fn origin(e: &rdy_engine::config_levels::Effective) -> (Value, Level) {
    let o = e.origins.iter().find(|o| o.key == KEY).expect("origin");
    (o.value.clone(), o.level)
}

#[test]
fn dgm5_project_base_path_defaults_and_validates() {
    // the key sits right after `description`, project level only, default `/`
    let keys: Vec<&str> = KEY_LEVELS.iter().map(|r| r.key).collect();
    let at = keys.iter().position(|k| *k == "description").unwrap();
    assert_eq!(keys[at + 1], KEY);
    let rule = &KEY_LEVELS[at + 1];
    assert_eq!(rule.levels, &[Level::Project]);
    assert_eq!(rule.default, Some("/"));

    // the closed rule
    let long = |n: usize| format!("/{}/", "a".repeat(n - 2));
    for ok in [
        "/",
        "/docs/",
        "/a/b/",
        "/a-b_c.d~e/",
        "/v1.2/",
        "/.../",
        "/..a/",
        "/A9/x/",
    ] {
        assert!(valid_site_base_path(ok), "{ok:?} is valid");
    }
    assert!(valid_site_base_path(&long(2048)), "2048 bytes is valid");
    assert!(!valid_site_base_path(&long(2049)), "2049 bytes is not");
    let rejected = [
        "",
        "docs",
        "/docs",
        "docs/",
        "//",
        "///",
        "/a//b/",
        "//a/",
        "/a//",
        "/./",
        "/../",
        "/a/./b/",
        "/a/../",
        "/a b/",
        " /a/",
        "/a/ ",
        "/a%20b/",
        "/a:b/",
        "/a\\b/",
        "/a?b/",
        "/a#b/",
        "/a;b/",
        "/a@b/",
        "/a\tb/",
        "/a\nb/",
        "/\u{e9}/",
        "/a\u{0}b/",
        "http://x/",
        "https://x.example/docs/",
    ];
    for bad in rejected {
        assert!(!valid_site_base_path(bad), "{bad:?} is not valid");
    }

    // unset: the default, from the default level; no warning
    let e = effective_of("pack: rdy-standard\n");
    assert_eq!(origin(&e), (Value::from("/"), Level::Default));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);

    // set: the project value wins
    let e = effective_of(&project_yaml("/docs/"));
    assert_eq!(origin(&e), (Value::from("/docs/"), Level::Project));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    let e = effective_of(&project_yaml(&long(2048)));
    assert_eq!(origin(&e), (Value::from(long(2048)), Level::Project));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);

    // a string outside the form warns once with the closed texts and reads as `/`
    for bad in rejected.iter().copied().chain([long(2049).as_str()]) {
        let e = effective_of(&project_yaml(bad));
        assert_eq!(e.warnings.len(), 1, "{bad:?}: {:?}", e.warnings);
        let w = &e.warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN", "{bad:?}");
        assert_eq!(w.message, DETAIL, "{bad:?}");
        assert_eq!(w.fix, FIX, "{bad:?}");
        assert_eq!(origin(&e).0, Value::from("/"), "{bad:?}");
        assert_eq!(
            e.merged
                .get("site")
                .and_then(Value::as_mapping)
                .and_then(|m| m.get("base_path")),
            Some(&Value::from("/")),
            "{bad:?}: the merged settings hold the default"
        );
    }

    // a new project's `.rdy.yaml` holds no `site` key, and an open project reads `/`
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let text = std::fs::read_to_string(d.path().join(".rdy.yaml")).unwrap();
    let parsed: Value = serde_yaml_ng::from_str(&text).unwrap();
    assert!(parsed.get("site").is_none(), "{text}");
    assert!(!text.contains("site"), "{text}");
    assert_eq!(r.read().site_base_path(), "/");
    drop(r);

    // an open project reads its configured value
    std::fs::write(d.path().join(".rdy.yaml"), project_yaml("/docs/")).unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().site_base_path(), "/docs/");
    drop(r);

    // an invalid value opens the project and reads as `/` (config mistakes warn)
    std::fs::write(d.path().join(".rdy.yaml"), project_yaml("docs")).unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().site_base_path(), "/");
}

#[test]
fn dgm5_base_path_is_project_only_and_unknown_keys_warn() {
    let d = tempfile::tempdir().unwrap();
    let project = d.path();
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    std::fs::write(project.join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    let local_yaml = "site:\n  base_path: /local/\n";
    std::fs::write(project.join(".rdy/local.yaml"), local_yaml).unwrap();
    let user_dir = tempfile::tempdir().unwrap();
    let user: &Path = &user_dir.path().join("config.yaml");
    std::fs::write(user, "site:\n  base_path: /user/\n").unwrap();

    // a local or user value is warned about and ignored
    let e = effective(project, Some(user)).unwrap();
    assert_eq!(origin(&e), (Value::from("/"), Level::Default));
    assert_eq!(e.warnings.len(), 2, "{:?}", e.warnings);
    let local = e
        .warnings
        .iter()
        .find(|w| w.message.contains(".rdy/local.yaml"))
        .expect("local warning");
    assert_eq!(local.code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        local.message,
        "site.base_path in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here"
    );
    assert_eq!(local.fix, "move site.base_path to .rdy.yaml");
    let person = e
        .warnings
        .iter()
        .find(|w| w.message.contains(&*user.display().to_string()))
        .expect("user warning");
    assert_eq!(person.code, "W_CONFIG_UNKNOWN");
    assert!(
        person
            .message
            .ends_with("belongs in .rdy.yaml; rdy ignores it here"),
        "{}",
        person.message
    );

    // the project value still wins over them
    std::fs::write(project.join(".rdy.yaml"), project_yaml("/docs/")).unwrap();
    let e = effective(project, Some(user)).unwrap();
    assert_eq!(origin(&e), (Value::from("/docs/"), Level::Project));
    assert_eq!(e.warnings.len(), 2);

    // an unknown `site` key warns with the nearest-key rule
    std::fs::remove_file(project.join(".rdy/local.yaml")).unwrap();
    std::fs::write(
        project.join(".rdy.yaml"),
        "pack: rdy-standard\nsite:\n  bse_path: /docs/\n  logo: x.png\n",
    )
    .unwrap();
    let e = effective(project, None).unwrap();
    assert_eq!(origin(&e), (Value::from("/"), Level::Default));
    assert_eq!(e.warnings.len(), 2, "{:?}", e.warnings);
    assert!(e.warnings.iter().all(|w| w.code == "W_CONFIG_UNKNOWN"));
    assert_eq!(
        e.warnings[0].message,
        "unknown key site.bse_path in .rdy.yaml (project level); rdy ignores it"
    );
    assert_eq!(
        e.warnings[0].fix,
        "did you mean site.base_path? rename site.bse_path in .rdy.yaml"
    );
    assert_eq!(
        e.warnings[1].message,
        "unknown key site.logo in .rdy.yaml (project level); rdy ignores it"
    );

    // a value that is not a string follows the existing type refusal
    std::fs::write(
        project.join(".rdy.yaml"),
        "pack: rdy-standard\nsite:\n  base_path: 5\n",
    )
    .unwrap();
    let err = effective(project, None).unwrap_err();
    assert_eq!(err.code, E_CONFIG);
    assert!(
        err.message
            .starts_with("site.base_path in .rdy.yaml: invalid type"),
        "{}",
        err.message
    );
    assert_eq!(err.fix, "fix site.base_path in .rdy.yaml");
}

fn writer() -> Session {
    Session {
        actor: "human:t".into(),
        run: Some("cli".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

#[test]
fn dgm5_snapshot_keeps_its_base_path_after_reload() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(d.path(), "rdy-standard").unwrap();
    assert_eq!(r.read().site_base_path(), "/");

    std::fs::write(d.path().join(".rdy.yaml"), project_yaml("/old/")).unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let old = r.read();
    assert_eq!(old.site_base_path(), "/old/");

    // a full reload yields the new value; the held snapshot keeps its own
    std::fs::write(d.path().join(".rdy.yaml"), project_yaml("/new/")).unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert_eq!(old.site_base_path(), "/old/");
    assert_eq!(r.read().site_base_path(), "/new/");

    // a later partial publication carries the new value
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}
    ]))
    .unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    assert_eq!(r.read().site_base_path(), "/new/");
    assert_eq!(old.site_base_path(), "/old/");
}
