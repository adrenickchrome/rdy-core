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

/// A path as a YAML scalar, quoted as the emitter needs, so Windows backslashes survive.
fn yaml(p: &str) -> String {
    serde_yaml_ng::to_string(p).unwrap().trim_end().to_string()
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
    let odd = std::path::Path::new(&odd)
        .join("Zoë Ünal")
        .to_str()
        .unwrap()
        .to_string();
    let (_d2, env2) = user_file(&format!("instructions:\n  person: {}\n", yaml(&odd)));
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
        let file = d
            .path()
            .join("rdy")
            .join("config.yaml")
            .display()
            .to_string();
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

#[test]
fn wx9_xdg_then_appdata_places_user_config() {
    let cfg = |base: &str| join(base, &["rdy", "config.yaml"]);
    // Windows: XDG first, with a drive path or a UNC path and spaces; else %APPDATA%
    let mut w = windows();
    assert_eq!(
        user_config_path(Platform::Windows, &w),
        Some(cfg(r"C:\Users\Zoë Ünal\AppData\Roaming"))
    );
    w.xdg_config_home = os(r"\\files\home dir\cfg");
    assert_eq!(
        user_config_path(Platform::Windows, &w),
        Some(cfg(r"\\files\home dir\cfg"))
    );
    // Windows ignores HOME for the config place, and with no APPDATA there is no place
    let only_home = PathEnv {
        home: os("/home/zoe"),
        ..PathEnv::default()
    };
    assert_eq!(user_config_path(Platform::Windows, &only_home), None);
    assert_eq!(
        user_config_path(Platform::Windows, &PathEnv::default()),
        None
    );
    // Unix ignores APPDATA
    let only_appdata = PathEnv {
        app_data: os("/data"),
        ..PathEnv::default()
    };
    assert_eq!(user_config_path(Platform::Unix, &only_appdata), None);
    // XDG moves the config and never the meta default, on both families
    for (platform, base) in [
        (Platform::Unix, unix("/home/zoe")),
        (Platform::Windows, windows()),
    ] {
        let mut moved = base.clone();
        moved.xdg_config_home = os(if platform == Platform::Unix {
            "/elsewhere/cfg"
        } else {
            r"E:\cfg"
        });
        assert_ne!(
            user_config_path(platform, &base),
            user_config_path(platform, &moved)
        );
        assert_eq!(
            default_meta_path(platform, &base),
            default_meta_path(platform, &moved),
            "{platform:?}"
        );
    }
}

#[test]
fn wx9_person_source_uses_profile_then_home_drive_path() {
    let (_d, mut env) = user_file("actor: human:zoe\n");
    env.home = None;
    let meta = |base: &str| PersonPath::Default(join(base, &[".rdy", "meta"]));
    // the profile first, with spaces, non-ASCII text and a UNC share
    for profile in [r"C:\Users\Zoë Ünal", r"\\files\users\Zoë Ünal"] {
        env.user_profile = os(profile);
        env.home_drive = os("D:");
        env.home_path = os(r"\Users\other");
        let got = resolve_person(Platform::Windows, &env).unwrap();
        assert_eq!(got.path, meta(profile), "{profile}");
    }
    // no usable profile: the home drive followed by the home path
    for profile in [None, os(""), os("relative"), os("C:Users"), os(r"\Users")] {
        env.user_profile = profile.clone();
        env.home_drive = os("D:");
        env.home_path = os(r"\Users\fall back");
        let got = resolve_person(Platform::Windows, &env).unwrap();
        assert_eq!(got.path, meta(r"D:\Users\fall back"), "{profile:?}");
    }
    // a drive-relative or rootless pair, or half a pair, is no home
    for (drive, path) in [
        (os("D:"), os("Users")),
        (os("D:"), None),
        (None, os(r"\Users")),
        (os(""), os("")),
        (os("D"), os(r"\Users")),
    ] {
        env.user_profile = None;
        env.home_drive = drive.clone();
        env.home_path = path.clone();
        let got = resolve_person(Platform::Windows, &env).unwrap();
        assert_eq!(got.path, PersonPath::Unavailable, "{drive:?} {path:?}");
    }
    // HOME is not a Windows home
    env.user_profile = None;
    env.home_drive = None;
    env.home_path = None;
    env.home = os("C:\\Users\\x");
    assert_eq!(
        resolve_person(Platform::Windows, &env).unwrap().path,
        PersonPath::Unavailable
    );
}

#[test]
fn wx9_person_source_text_that_is_not_unicode_is_data() {
    let odd = rdy_store::pio::not_unicode(if cfg!(windows) {
        r"C:\Users\zo"
    } else {
        "/home/zo"
    });
    let (_d, mut env) = user_file("actor: human:zoe\n");
    env.home = Some(odd.clone());
    env.user_profile = Some(odd.clone());
    let got = resolve_person(Platform::HOST, &env).unwrap();
    assert_eq!(
        got.path,
        PersonPath::Default(PathBuf::from(&odd).join(".rdy").join("meta"))
    );
}

#[test]
fn wx9_person_override_is_absolute_user_only() {
    let (_p, target) = abs_dir();
    // an absolute value is used as written, from the user config alone
    let (d, env) = user_file(&format!("instructions:\n  person: {}\n", yaml(&target)));
    let got = resolve_person(Platform::HOST, &env).unwrap();
    assert_eq!(got.path, PersonPath::Override(PathBuf::from(&target)));
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
    // a project file and a local file cannot redirect it: wrong level, a warning, no origin
    let proj = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(proj.path().join(".rdy")).unwrap();
    let text = format!("instructions:\n  person: {}\n", yaml(&target));
    std::fs::write(
        proj.path().join(PROJECT_FILE),
        format!("pack: rdy-standard\n{text}"),
    )
    .unwrap();
    std::fs::write(proj.path().join(".rdy/local.yaml"), &text).unwrap();
    let eff = rdy_engine::config_levels::effective(proj.path(), None).unwrap();
    assert!(eff.origins.iter().all(|o| o.key != "instructions.person"));
    let wrong = eff
        .warnings
        .iter()
        .filter(|w| w.message.contains("instructions.person"))
        .count();
    assert_eq!(wrong, 2, "{:?}", eff.warnings);
    // with an empty user config, the person source is the default whatever the project holds
    let (_d2, env2) = user_file("");
    assert!(matches!(
        resolve_person(Platform::Unix, &env2).unwrap().path,
        PersonPath::Default(_)
    ));
    drop(d);
    // relative and rooted-without-drive values are not absolute on any host: warn, keep default
    let mut bad = vec!["relative\\dir", "..\\up"];
    if cfg!(windows) {
        bad.extend([r"C:meta", r"\meta"]);
    }
    for value in bad {
        let (_d, env) = user_file(&format!("instructions:\n  person: {}\n", yaml(value)));
        let got = resolve_person(Platform::HOST, &env).unwrap();
        assert!(matches!(
            got.path,
            PersonPath::Unavailable | PersonPath::Default(_)
        ));
        assert!(got.origin.is_none(), "{value}");
        assert_eq!(got.warnings.len(), 1, "{value}: {:?}", got.warnings);
    }
}

#[cfg(windows)]
#[test]
fn wx9_person_override_accepts_a_unc_share_with_spaces() {
    let value = r"\\files\team share\Zoë Ünal\meta";
    let (_d, env) = user_file(&format!("instructions:\n  person: {}\n", yaml(value)));
    let got = resolve_person(Platform::Windows, &env).unwrap();
    assert_eq!(got.path, PersonPath::Override(PathBuf::from(value)));
}
