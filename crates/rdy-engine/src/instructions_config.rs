//! Where the person's instructions live (AIN, D-FPKPS for the Windows config place). The person
//! layer is an ordinary rdy project, the "meta" project. Its default place is `<home>/.rdy/meta`.
//! Only `instructions.person` in the user config moves it, and only to an absolute path that rdy
//! uses as written: no `~`, no environment variable. The project config and `.rdy/local.yaml`
//! never choose it, so no project file is read here.
//!
//! The platform and the environment values are data, so tests need no process environment.
//! `XDG_CONFIG_HOME` moves the user config, never the default meta project. A default needs an
//! absolute home: with none, [`PersonPath::Unavailable`] says so, and nothing resolves against
//! the working directory.

use crate::RdyError;
use crate::config_levels::{self, Origin};
use rdy_schema::Issue;
use serde_yaml_ng::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The key that moves the person source, user config only.
pub const PERSON_KEY: &str = "instructions.person";

/// The message when no home directory and no valid override give a person source.
pub const NO_HOME_MESSAGE: &str = "No home directory is available; set instructions.person to an absolute project path in user config.";

/// The path rules of one platform family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

impl Platform {
    /// The platform this binary runs on.
    pub const HOST: Platform = if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Unix
    };
}

/// The environment values that place the user config and the default meta project.
#[derive(Clone, Debug, Default)]
pub struct PathEnv {
    pub xdg_config_home: Option<OsString>,
    pub home: Option<OsString>,
    pub user_profile: Option<OsString>,
    pub home_drive: Option<OsString>,
    pub home_path: Option<OsString>,
    pub app_data: Option<OsString>,
}

impl PathEnv {
    /// The values of this process. The one place the person source reads the environment.
    pub fn from_process() -> Self {
        let get = |k: &str| std::env::var_os(k);
        PathEnv {
            xdg_config_home: get("XDG_CONFIG_HOME"),
            home: get("HOME"),
            user_profile: get("USERPROFILE"),
            home_drive: get("HOMEDRIVE"),
            home_path: get("HOMEPATH"),
            app_data: get("APPDATA"),
        }
    }
}

/// Whether `p` is absolute under `platform`'s rules. The host's own rule is `Path::is_absolute`;
/// for the other family the text decides, so a Windows rule can be tested on any host.
fn is_absolute(platform: Platform, p: &Path) -> bool {
    if platform == Platform::HOST {
        return p.is_absolute();
    }
    let Some(s) = p.to_str() else {
        return false;
    };
    match platform {
        Platform::Unix => s.starts_with('/'),
        Platform::Windows => {
            let b = s.as_bytes();
            let drive = b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'\\' || b[2] == b'/');
            drive || s.starts_with("\\\\")
        }
    }
}

/// The home directory: absolute, else `None`. Windows takes the profile, else the home drive
/// followed by the home path.
fn home_dir(platform: Platform, env: &PathEnv) -> Option<PathBuf> {
    let candidate = match platform {
        Platform::Unix => env.home.as_ref().map(PathBuf::from),
        Platform::Windows => env
            .user_profile
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| is_absolute(platform, p))
            .or_else(|| {
                let mut joined = env.home_drive.clone()?;
                joined.push(env.home_path.as_ref()?);
                Some(PathBuf::from(joined))
            }),
    };
    candidate.filter(|p| is_absolute(platform, p))
}

/// The default meta project: `<home>/.rdy/meta`, `None` without an absolute home.
pub fn default_meta_path(platform: Platform, env: &PathEnv) -> Option<PathBuf> {
    home_dir(platform, env).map(|h| h.join(".rdy").join("meta"))
}

/// The user config file. `XDG_CONFIG_HOME` first on both families, then `$HOME/.config` on Unix
/// and `%APPDATA%` on Windows (D-FPKPS).
pub fn user_config_path(platform: Platform, env: &PathEnv) -> Option<PathBuf> {
    if let Some(x) = &env.xdg_config_home {
        return Some(Path::new(x).join("rdy").join("config.yaml"));
    }
    match platform {
        Platform::Unix => env
            .home
            .as_ref()
            .map(|h| Path::new(h).join(".config").join("rdy").join("config.yaml")),
        Platform::Windows => env
            .app_data
            .as_ref()
            .map(|a| Path::new(a).join("rdy").join("config.yaml")),
    }
}

/// Where the person's project is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonPath {
    /// `instructions.person` of the user config, as written.
    Override(PathBuf),
    /// `<home>/.rdy/meta`.
    Default(PathBuf),
    /// No valid override and no absolute home; [`NO_HOME_MESSAGE`] says so.
    Unavailable,
}

/// The person source with what the user-config read found.
#[derive(Clone, Debug)]
pub struct PersonSource {
    pub path: PersonPath,
    /// The origin row of `instructions.person`, when a valid value set it.
    pub origin: Option<Origin>,
    /// Every warning of the user config read (`W_CONFIG_UNKNOWN`), in the order it found them.
    pub warnings: Vec<Issue>,
    /// The user config file the read used, when a place for it exists.
    pub config_file: Option<PathBuf>,
}

/// The person source of `env` on `platform`. It reads the user config alone, no project, and keeps
/// the config refusals (`E_CONFIG`, `E_LIMIT`, `E_IO`). A key typo, a value in the wrong level
/// and a bad `instructions.person` warn, and the default applies.
pub fn resolve_person(platform: Platform, env: &PathEnv) -> Result<PersonSource, RdyError> {
    let config_file = user_config_path(platform, env);
    let effective = config_levels::user_only(config_file.as_deref())?;
    let origin = effective
        .origins
        .iter()
        .find(|o| o.key == PERSON_KEY)
        .cloned();
    let path = match origin.as_ref().and_then(|o| o.value.as_str()) {
        Some(p) => PersonPath::Override(PathBuf::from(p)),
        None => match default_meta_path(platform, env) {
            Some(d) => PersonPath::Default(d),
            None => PersonPath::Unavailable,
        },
    };
    Ok(PersonSource {
        path,
        origin,
        warnings: effective.warnings,
        config_file,
    })
}

/// [`resolve_person`] over this process.
pub fn resolve_person_from_env() -> Result<PersonSource, RdyError> {
    resolve_person(Platform::HOST, &PathEnv::from_process())
}

/// The explicit value of `instructions.person` in a user-config mapping when it is a usable
/// absolute path.
pub(crate) fn usable_person(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && Path::new(s).is_absolute())
}
