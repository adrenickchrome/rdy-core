//! ain2: the person's instructions come from a project the user config can move. The default is
//! `<home>/.rdy/meta`; only the user config's `instructions.person` (an absolute path, used as
//! written) overrides it. Paths arrive as data: no test changes the process environment.
use rdy_engine::config_levels::{Level, PROJECT_FILE};
use rdy_engine::instructions_config::{
    NO_HOME_MESSAGE, PathEnv, PersonPath, Platform, default_meta_path, resolve_person,
    user_config_path,
};
use rdy_engine::{E_CONFIG, E_IO};
use std::ffi::OsString;
use std::path::PathBuf;

fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
}

fn unix(home: &str) -> PathEnv {
    PathEnv {
        home: os(home),
        ..PathEnv::default()
    }
}

fn windows() -> PathEnv {
    PathEnv {
        user_profile: os(r"C:\Users\Zoë Ünal"),
        app_data: os(r"C:\Users\Zoë Ünal\AppData\Roaming"),
        ..PathEnv::default()
    }
}

fn join(base: &str, parts: &[&str]) -> PathBuf {
    parts.iter().fold(PathBuf::from(base), |p, s| p.join(s))
}

#[test]
fn ain2_meta_path_is_home_based_on_each_platform() {
    // Unix: the home, whatever XDG says; spaces and non-ASCII stay as they are
    let mut env = unix("/home/with space/Zoë");
    env.xdg_config_home = os("/elsewhere/cfg");
    assert_eq!(
        default_meta_path(Platform::Unix, &env),
        Some(join("/home/with space/Zoë", &[".rdy", "meta"]))
    );
    // Windows: the profile, else the home drive plus the home path
    assert_eq!(
        default_meta_path(Platform::Windows, &windows()),
        Some(join(r"C:\Users\Zoë Ünal", &[".rdy", "meta"]))
    );
    let fallback = PathEnv {
        home_drive: os("D:"),
        home_path: os(r"\Users\fallback"),
        ..PathEnv::default()
    };
    assert_eq!(
        default_meta_path(Platform::Windows, &fallback),
        Some(join(r"D:\Users\fallback", &[".rdy", "meta"]))
    );
    // a profile that is not absolute yields to the fallback pair
    let both = PathEnv {
        user_profile: os("relative"),
        ..fallback.clone()
    };
    assert_eq!(
        default_meta_path(Platform::Windows, &both),
        Some(join(r"D:\Users\fallback", &[".rdy", "meta"]))
    );
    // the user config: XDG, else the platform's own place
    assert_eq!(
        user_config_path(Platform::Unix, &env),
        Some(join("/elsewhere/cfg", &["rdy", "config.yaml"]))
    );
    assert_eq!(
        user_config_path(Platform::Unix, &unix("/home/a")),
        Some(join("/home/a", &[".config", "rdy", "config.yaml"]))
    );
    assert_eq!(
        user_config_path(Platform::Windows, &windows()),
        Some(join(
            r"C:\Users\Zoë Ünal\AppData\Roaming",
            &["rdy", "config.yaml"]
        ))
    );
    let mut w = windows();
    w.xdg_config_home = os(r"E:\cfg");
    assert_eq!(
        user_config_path(Platform::Windows, &w),
        Some(join(r"E:\cfg", &["rdy", "config.yaml"]))
    );
}

fn user_file(text: &str) -> (tempfile::TempDir, PathEnv) {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("rdy");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.yaml"), text).unwrap();
    let env = PathEnv {
        xdg_config_home: Some(d.path().as_os_str().to_owned()),
        home: os("/home/zoe"),
        ..PathEnv::default()
    };
    (d, env)
}

fn abs_dir() -> (tempfile::TempDir, String) {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().to_str().unwrap().to_string();
    (d, s)
}

#[test]
fn ain2_person_override_is_absolute_and_user_only() {
    let (_p, target) = abs_dir();
    let (_d, env) = user_file(&format!("instructions:\n  person: '{target}'\n"));
    let got = resolve_person(Platform::HOST, &env).unwrap();
    assert_eq!(got.path, PersonPath::Override(PathBuf::from(&target)));
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
    let o = got.origin.expect("the key has an origin");
    assert_eq!(o.key, "instructions.person");
    assert_eq!(o.level, Level::User);
    assert_eq!(o.value.as_str(), Some(target.as_str()));
    // XDG moved the config but not the default; a space and non-ASCII survive
    let (_q, odd) = abs_dir();
    let odd = format!("{odd}/Zoë Ünal");
    let (_d2, env2) = user_file(&format!("instructions:\n  person: \"{odd}\"\n"));
    assert_eq!(
        resolve_person(Platform::HOST, &env2).unwrap().path,
        PersonPath::Override(PathBuf::from(&odd))
    );
    // no override: the home default
    let (_d3, env3) = user_file("actor: human:zoe\n");
    assert_eq!(
        resolve_person(Platform::Unix, &env3).unwrap().path,
        PersonPath::Default(join("/home/zoe", &[".rdy", "meta"]))
    );
    // a project file cannot redirect: it is wrong-level, warns, and the default stays
    let (_d4, env4) = user_file("");
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join(PROJECT_FILE),
        format!("pack: rdy-standard\ninstructions:\n  person: '{target}'\n"),
    )
    .unwrap();
    let got = resolve_person(Platform::Unix, &env4).unwrap();
    assert_eq!(
        got.path,
        PersonPath::Default(join("/home/zoe", &[".rdy", "meta"]))
    );
    let eff = rdy_engine::config_levels::effective(proj.path(), None).unwrap();
    let w = eff
        .warnings
        .iter()
        .find(|w| w.message.contains("instructions.person"))
        .expect("wrong-level warning");
    assert_eq!(w.code, "W_CONFIG_UNKNOWN");
    assert!(w.message.contains("belongs in the user config"), "{w:?}");
    assert!(eff.origins.iter().all(|o| o.key != "instructions.person"));
}

#[test]
fn ain2_config_typos_warn_and_keep_the_default() {
    let default = PersonPath::Default(join("/home/zoe", &[".rdy", "meta"]));
    for (text, needle, near) in [
        (
            "instrucions:\n  person: /x\n",
            "instrucions",
            "instructions.person",
        ),
        (
            "instructions:\n  persn: /x\n",
            "instructions.persn",
            "instructions.person",
        ),
        (
            "instructions.person: /x\n",
            "instructions.person",
            "instructions:",
        ),
    ] {
        let (_d, env) = user_file(text);
        let got = resolve_person(Platform::Unix, &env).unwrap();
        assert_eq!(got.path, default, "{text}");
        assert!(got.origin.is_none(), "{text}");
        let w = got
            .warnings
            .iter()
            .find(|w| w.message.contains(needle))
            .unwrap_or_else(|| panic!("{text}: {:?}", got.warnings));
        assert_eq!(w.code, "W_CONFIG_UNKNOWN", "{text}");
        assert!(w.fix.contains(near), "{text}: {}", w.fix);
    }
    // relative, blank, tilde and non-text values: one warning with the decided words
    for value in [
        "relative/dir",
        "''",
        "'~/meta'",
        "'~'",
        "5",
        "[a, b]",
        "true",
    ] {
        let (d, env) = user_file(&format!("instructions:\n  person: {value}\n"));
        let file = d.path().join("rdy/config.yaml").display().to_string();
        let got = resolve_person(Platform::Unix, &env).unwrap();
        assert_eq!(got.path, default, "{value}");
        assert!(got.origin.is_none(), "{value}");
        assert_eq!(got.warnings.len(), 1, "{value}: {:?}", got.warnings);
        let w = &got.warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            w.message,
            format!("instructions.person in {file} must be an absolute path; rdy ignores it"),
            "{value}"
        );
        assert_eq!(
            w.fix,
            format!("set instructions.person to an absolute project path in {file}"),
            "{value}"
        );
    }
    // the existing refusals stay refusals
    let (_d, env) = user_file("instructions: [unclosed\n");
    assert_eq!(
        resolve_person(Platform::Unix, &env).unwrap_err().code,
        E_CONFIG
    );
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("rdy/config.yaml")).unwrap();
    let env = PathEnv {
        xdg_config_home: Some(d.path().as_os_str().to_owned()),
        home: os("/home/zoe"),
        ..PathEnv::default()
    };
    assert_eq!(resolve_person(Platform::Unix, &env).unwrap_err().code, E_IO);
}

#[test]
fn ain2_missing_home_never_uses_the_working_directory() {
    for platform in [Platform::Unix, Platform::Windows] {
        for env in [
            PathEnv::default(),
            PathEnv {
                home: os("~"),
                user_profile: os("~"),
                ..PathEnv::default()
            },
            PathEnv {
                home: os(""),
                user_profile: os("relative/home"),
                home_drive: os("C:"),
                home_path: os("no-leading-slash"),
                ..PathEnv::default()
            },
        ] {
            assert_eq!(default_meta_path(platform, &env), None, "{platform:?}");
            let got = resolve_person(platform, &env).unwrap();
            assert_eq!(got.path, PersonPath::Unavailable, "{platform:?}");
            assert!(got.origin.is_none());
        }
    }
    assert_eq!(
        NO_HOME_MESSAGE,
        "No home directory is available; set instructions.person to an absolute project path in user config."
    );
    // an XDG config can still supply the override when there is no home
    let (_p, target) = abs_dir();
    let (_d, mut env) = user_file(&format!("instructions:\n  person: '{target}'\n"));
    env.home = None;
    assert_eq!(
        resolve_person(Platform::HOST, &env).unwrap().path,
        PersonPath::Override(PathBuf::from(target))
    );
}

#[test]
fn ain2_config_origin_and_warnings_survive_resolution() {
    let (_p, target) = abs_dir();
    // a valid override beside an unrelated typo: both survive
    let (d, env) = user_file(&format!(
        "instructions:\n  person: '{target}'\ndecision:\n  home: docs\n"
    ));
    let file = d.path().join("rdy/config.yaml");
    let got = resolve_person(Platform::HOST, &env).unwrap();
    let o = got.origin.unwrap();
    assert_eq!(o.file.as_deref(), Some(file.as_path()));
    assert!(
        got.warnings
            .iter()
            .any(|w| w.code == "W_CONFIG_UNKNOWN" && w.message.contains("decision.home")),
        "{:?}",
        got.warnings
    );
    assert_eq!(got.config_file.as_deref(), Some(file.as_path()));
    // the key is listed once, last in the table, at the user level only
    let rule = rdy_engine::config_levels::KEY_LEVELS.last().unwrap();
    assert_eq!(rule.key, "instructions.person");
    assert_eq!(rule.levels, &[Level::User]);
    // a missing config file is no error and no warning
    let d = tempfile::tempdir().unwrap();
    let env = PathEnv {
        xdg_config_home: Some(d.path().as_os_str().to_owned()),
        home: os("/home/zoe"),
        ..PathEnv::default()
    };
    let got = resolve_person(Platform::Unix, &env).unwrap();
    assert!(got.warnings.is_empty() && got.origin.is_none());
}
