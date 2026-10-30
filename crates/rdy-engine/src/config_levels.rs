//! Settings come from four levels (FW-X5, D-C5955): `.rdy/local.yaml`, the project config
//! (`.rdy.yaml` at the project root), the user config and the defaults. The
//! most specific level that sets a key wins. Each key of [`KEY_LEVELS`] names the levels that may
//! set it (R-P6); a key in a file of another level is a `W_CONFIG_UNKNOWN` warning that names the
//! file it belongs in, and rdy ignores its value. Keys outside the table are kept as they are and
//! never shown; each one is a `W_CONFIG_UNKNOWN` warning naming the key, the file and the level
//! (R-P6). A mapping key that holds a `.` is never known: rdy reads only nested keys. An unknown
//! key meant as a review setting gets its own warning text (#832): a dotted `reviews.` key, one
//! under `reviews`, and one nearest to a `reviews` key. The text says that the review gate would
//! stay off. Founder ruling (config typos warn): every config mistake is a warning, never
//! `E_CONFIG`, and the project opens. Only a wrong value type on a known key in a file that
//! allows it is `E_CONFIG`.
//!
//! One key merges name by name, not file by file: `code_paths` (FW-X5, F1). `code_repos` in
//! `.rdy.yaml` names the linked code repos; each name's checkout path is its `code_paths` entry in
//! `.rdy/local.yaml`, else its entry in the user config ([`linked_repos`]). So `code_paths` has
//! one origin per entry that applies (`code_paths.<name>`, from the file that supplied it), and
//! `merged` holds those entries as one mapping.
//!
//! `remotes` and `hub.installs` merge entry by entry too (`hub_config`): one row per remote and
//! one per install field. Unlike other unknown keys, an unknown, wrong-level or invalid hub
//! setting is dropped, and its warning never quotes a value. Every file is bounded (1 MiB, depth
//! 32, 4096 entries, 16384-byte scalars, no alias) before it is parsed, and a file that does not
//! parse names only the file.

use crate::hub_types::{LimitUnit, MAX_CONFIG_BYTES};
use crate::{E_CONFIG, E_IO, E_NO_PROJECT, RdyError};
use rdy_schema::{Issue, Severity};
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// The project config at the project root (FW-X5, D-C5955), read by `read_config`,
/// `parse_config`, `serve_config`, `pack_fingerprint` and `code_repos`. rdy reads no other
/// project file (D-W7RBV).
pub const PROJECT_FILE: &str = ".rdy.yaml";

/// Whether `project` is a project: it has `.rdy.yaml` and an entry at `.rdy/store` (the store
/// checks that entry when it opens).
pub fn is_project(project: &Path) -> bool {
    project.join(PROJECT_FILE).exists() && std::fs::symlink_metadata(store_path(project)).is_ok()
}

/// Whether `.rdy/store` holds anything: a directory with an entry in it, or anything else
/// that stands at that path. `rdy init` never adopts such a store.
pub fn store_has_entry(project: &Path) -> bool {
    let p = store_path(project);
    match std::fs::read_dir(&p) {
        Ok(mut d) => d.next().is_some(),
        Err(_) => std::fs::symlink_metadata(&p).is_ok(),
    }
}

fn store_path(project: &Path) -> PathBuf {
    project.join(".rdy").join(rdy_store::layout::STORE_DIR)
}

/// `E_NO_PROJECT` unless `project` is a project ([`is_project`]), in the words of
/// [`missing_project`]: the engine's open and the CLI check this before they open a store.
pub fn require_project(project: &Path) -> Result<(), RdyError> {
    if is_project(project) {
        return Ok(());
    }
    Err(missing_project(project))
}

/// The error for `project` when it is not a project. A directory with `.rdy.yaml` and no store
/// says so, and `rdy init` gives the same text there. Otherwise, a directory with no `.rdy`
/// directory is not a project, and one with `.rdy` and no `.rdy.yaml` names the missing file.
pub fn missing_project(project: &Path) -> RdyError {
    if project.join(PROJECT_FILE).exists() {
        RdyError::new(
            E_NO_PROJECT,
            format!("{} has .rdy.yaml but no store", project.display()),
            "move .rdy.yaml aside, run rdy init, then copy your settings back into the new .rdy.yaml",
        )
    } else if std::fs::metadata(project.join(".rdy")).is_ok_and(|m| m.is_dir()) {
        RdyError::new(
            E_NO_PROJECT,
            format!("{} has no .rdy.yaml, the project config", project.display()),
            "create .rdy.yaml in the project directory with the line `pack: rdy-standard` (or the pack this project uses)",
        )
    } else {
        RdyError::new(
            E_NO_PROJECT,
            format!("{} is not an rdy project", project.display()),
            "run rdy init in the project directory",
        )
    }
}

/// The settings of this checkout only, never committed.
pub const LOCAL_FILE: &str = ".rdy/local.yaml";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Local,
    Project,
    User,
    Default,
}

/// The levels that read files, most specific first.
const FILE_LEVELS: [Level; 3] = [Level::Local, Level::Project, Level::User];

/// One settable key: its dotted leaf path, the levels that may set it and its default (YAML
/// text; `None` when the key has no default).
pub struct KeyRule {
    pub key: &'static str,
    pub levels: &'static [Level],
    /// The type of the value, as `Config` reads it.
    pub ty: Ty,
    pub default: Option<&'static str>,
}

/// The type a key's value must have. `Opt*` also allows an empty value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Str,
    OptStr,
    Bool,
    U32,
    OptU64,
    /// A whole number (a YAML integer, any sign). A whole number outside 0 to 100 is not a wrong
    /// type: `range_warnings` warns and drops it.
    Percent,
    /// A whole number (a YAML integer, any sign). A whole number outside 1024 to 65535 is not a
    /// wrong type: `range_warnings` warns and drops it.
    Port,
    /// A string. A string that is not a private IPv4 address is not a wrong type:
    /// `range_warnings` warns and drops it.
    LanAddress,
    /// A whole number (a YAML integer, any sign). A whole number outside 1 to 1000 is not a
    /// wrong type: `range_warnings` warns and sets the key's default at that level.
    Count,
    /// A string. A string that is not one of the choices is not a wrong type: `range_warnings`
    /// warns and sets the key's default at that level.
    Choice(&'static [&'static str]),
    /// A string. A string outside the form of [`valid_site_base_path`] is not a wrong type:
    /// `range_warnings` warns and sets the key's default at that level.
    BasePath,
    /// Not read by `Config` yet: any value.
    Any,
}

impl Ty {
    /// `Ok` when `v` has this type, else the message of the deserializer.
    fn check(self, v: &Value) -> Result<(), String> {
        let r = match self {
            Ty::Any => return Ok(()),
            Ty::Str | Ty::BasePath => serde_yaml_ng::from_value::<String>(v.clone()).map(drop),
            Ty::OptStr => serde_yaml_ng::from_value::<Option<String>>(v.clone()).map(drop),
            Ty::Bool => serde_yaml_ng::from_value::<bool>(v.clone()).map(drop),
            Ty::U32 => serde_yaml_ng::from_value::<u32>(v.clone()).map(drop),
            Ty::OptU64 => serde_yaml_ng::from_value::<Option<u64>>(v.clone()).map(drop),
            Ty::Percent => {
                return match v {
                    Value::Number(n) if n.is_i64() || n.is_u64() => Ok(()),
                    other => Err(format!(
                        "invalid type: {}, expected a whole number from 0 to 100",
                        flow(other)
                    )),
                };
            }
            Ty::Port => {
                return match v {
                    Value::Number(n) if n.is_i64() || n.is_u64() => Ok(()),
                    other => Err(format!(
                        "invalid type: {}, expected a whole number from 1024 to 65535",
                        flow(other)
                    )),
                };
            }
            Ty::LanAddress => {
                return match v {
                    Value::String(_) => Ok(()),
                    other => Err(format!(
                        "invalid type: {}, expected a private IPv4 address",
                        flow(other)
                    )),
                };
            }
            Ty::Count => {
                return match v {
                    Value::Number(n) if n.is_i64() || n.is_u64() => Ok(()),
                    other => Err(format!(
                        "invalid type: {}, expected a whole number from 1 to 1000",
                        flow(other)
                    )),
                };
            }
            Ty::Choice(choices) => {
                return match v {
                    Value::String(_) => Ok(()),
                    other => Err(format!(
                        "invalid type: {}, expected one of {}",
                        flow(other),
                        choice_list(choices)
                    )),
                };
            }
        };
        r.map_err(|e| e.to_string())
    }
}

/// The choices of `propose.cadence` (FW-PC1), in the order the docs give them.
pub const PROPOSE_CADENCES: &[&str] = &["break", "task", "changes", "asked"];

/// The choices of `coverage.gate` (D-ZWP7Q), in the order the docs give them.
pub const COVERAGE_GATES: &[&str] = &["off", "no_lower"];

/// `choices` as a message names them: `a, b or c`.
fn choice_list(choices: &[&str]) -> String {
    match choices {
        [] => String::new(),
        [only] => (*only).to_string(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

const PROJECT_ONLY: &[Level] = &[Level::Project];
const ANY: &[Level] = &[Level::Local, Level::Project, Level::User];
const PERSON: &[Level] = &[Level::Local, Level::User];

/// Every key the levels know, in the order `rdy config` lists them (R-P6).
pub const KEY_LEVELS: &[KeyRule] = &[
    KeyRule {
        key: "pack",
        levels: PROJECT_ONLY,
        ty: Ty::Str,
        default: None,
    },
    KeyRule {
        key: "name",
        levels: PROJECT_ONLY,
        ty: Ty::OptStr,
        default: None,
    },
    KeyRule {
        key: "description",
        levels: PROJECT_ONLY,
        ty: Ty::OptStr,
        default: None,
    },
    KeyRule {
        key: SITE_BASE_PATH,
        levels: PROJECT_ONLY,
        ty: Ty::BasePath,
        default: Some("/"),
    },
    KeyRule {
        key: "reviews.require_up_to_date",
        levels: PROJECT_ONLY,
        ty: Ty::Bool,
        default: Some("false"),
    },
    KeyRule {
        key: "reviews.lease_ttl",
        levels: PROJECT_ONLY,
        ty: Ty::U32,
        default: Some("10"),
    },
    KeyRule {
        key: "decisions.home",
        levels: PROJECT_ONLY,
        ty: Ty::Any,
        default: None,
    },
    KeyRule {
        key: "code_repos",
        levels: PROJECT_ONLY,
        ty: Ty::Any,
        default: None,
    },
    KeyRule {
        key: CODE_PATHS_KEY,
        levels: PERSON,
        ty: Ty::Any,
        default: None,
    },
    KeyRule {
        key: "coverage.target",
        levels: PROJECT_ONLY,
        ty: Ty::Percent,
        default: Some("80"),
    },
    KeyRule {
        key: "coverage.gate",
        levels: PROJECT_ONLY,
        ty: Ty::Choice(COVERAGE_GATES),
        default: Some("off"),
    },
    KeyRule {
        key: "views.idle_days",
        levels: ANY,
        ty: Ty::U32,
        default: Some("14"),
    },
    KeyRule {
        key: "serve.start_timeout",
        levels: ANY,
        ty: Ty::OptU64,
        default: Some("30"),
    },
    KeyRule {
        key: "serve.idle_timeout",
        levels: ANY,
        ty: Ty::OptU64,
        default: Some("600"),
    },
    KeyRule {
        key: "serve.site_port",
        levels: ANY,
        ty: Ty::Port,
        default: None,
    },
    KeyRule {
        key: "serve.site_lan",
        levels: ANY,
        ty: Ty::Bool,
        default: Some("false"),
    },
    KeyRule {
        key: "serve.site_lan_address",
        levels: PERSON,
        ty: Ty::LanAddress,
        default: None,
    },
    KeyRule {
        key: "propose.cadence",
        levels: ANY,
        ty: Ty::Choice(PROPOSE_CADENCES),
        default: Some("break"),
    },
    KeyRule {
        key: "propose.changes",
        levels: ANY,
        ty: Ty::Count,
        default: Some("10"),
    },
    // the default is the bounded fallback of `$USER`, taken from the environment in `effective`
    KeyRule {
        key: ACTOR_KEY,
        levels: PERSON,
        ty: Ty::Str,
        default: None,
    },
    // signing (D-QVH4N): local and user levels only; `signing_config` reads them
    KeyRule {
        key: SIGNING_KEY,
        levels: PERSON,
        ty: Ty::OptStr,
        default: None,
    },
    KeyRule {
        key: SIGNING_AGENT_KEY,
        levels: PERSON,
        ty: Ty::OptStr,
        default: None,
    },
    KeyRule {
        key: SIGNING_SIGN,
        levels: PERSON,
        ty: Ty::Choice(SIGN_MODES),
        default: Some("auto"),
    },
    KeyRule {
        key: SIGNING_ALLOWED_SIGNERS,
        levels: PERSON,
        ty: Ty::OptStr,
        default: None,
    },
    // remotes (D-B8N21): one entry per name at any level, the most specific whole entry wins;
    // `hub_config` checks and merges them, one row per remote
    KeyRule {
        key: crate::hub_config::REMOTES_KEY,
        levels: ANY,
        ty: Ty::Any,
        default: None,
    },
    // install settings (n-XZDQD): ca_file in the user config only, pinned_origins in the user
    // config or .rdy/local.yaml, never .rdy.yaml; one row per install field
    KeyRule {
        key: crate::hub_config::HUB_INSTALLS_KEY,
        levels: PERSON,
        ty: Ty::Any,
        default: None,
    },
];

/// The static site's base path (D-94BT2), by its dotted key.
pub const SITE_BASE_PATH: &str = "site.base_path";

/// The most bytes of a `site.base_path`.
const SITE_BASE_PATH_MAX: usize = 2048;

/// Whether `value` is a usable `site.base_path`: `/`, or a path of 2 to 2048 UTF-8 bytes that
/// begins and ends with one slash. Its segments are not empty, are not `.` or `..`, and hold only
/// ASCII letters, digits, `-`, `_`, `.` and `~`. No URL parser is needed for that closed form
/// (no percent escape, colon, backslash, query, fragment, space or non-ASCII byte).
pub fn valid_site_base_path(value: &str) -> bool {
    if value == "/" {
        return true;
    }
    if value.len() < 2 || value.len() > SITE_BASE_PATH_MAX {
        return false;
    }
    let Some(inner) = value
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
    else {
        return false;
    };
    inner.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
    })
}

/// The signing settings, by their dotted keys.
pub const SIGNING_KEY: &str = "signing.key";
pub const SIGNING_AGENT_KEY: &str = "signing.agent_key";
pub const SIGNING_SIGN: &str = "signing.sign";
pub const SIGNING_ALLOWED_SIGNERS: &str = "signing.allowed_signers";

/// The choices of `signing.sign`.
pub const SIGN_MODES: &[&str] = &["never", "auto", "always"];

/// One key's winning value and where it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Origin {
    pub key: String,
    pub value: Value,
    pub level: Level,
    /// The file the value is in; `None` for a default.
    pub file: Option<PathBuf>,
}

impl Origin {
    /// The value as one line of YAML (a list or a map in flow style).
    pub fn value_line(&self) -> String {
        flow(&self.value)
    }
}

/// The settings of a project after the levels are applied.
#[derive(Clone, Debug)]
pub struct Effective {
    /// One entry per key of [`KEY_LEVELS`] that has a value, in table order, defaults included.
    /// `code_paths` instead has one entry per entry that applies (F1): key `code_paths.<name>`,
    /// the path as written, and the level and file that supplied it, sorted by name.
    pub origins: Vec<Origin>,
    /// What `Config` is read from: the project file's keys as they are, each key of the table
    /// replaced by its winning value (a default is not written in; the accessors have it).
    /// `code_paths` is the by-name merge: every entry that applies, as written.
    pub merged: Mapping,
    /// One `W_CONFIG_UNKNOWN` per key outside the table, in file precedence order.
    pub warnings: Vec<Issue>,
}

fn flow(v: &Value) -> String {
    match v {
        Value::Sequence(items) => {
            let parts: Vec<String> = items.iter().map(flow).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Mapping(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", flow(k), flow(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        scalar => serde_yaml_ng::to_string(scalar)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}

/// The value at a dotted `path` of `map`.
fn get_path<'a>(map: &'a Mapping, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut cur = map.get(parts.next()?)?;
    for part in parts {
        cur = cur.as_mapping()?.get(part)?;
    }
    Some(cur)
}

/// Sets the value at a dotted `path`, making each mapping on the way; a parent that holds
/// something else is replaced.
fn set_path(map: &mut Mapping, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            map.insert(Value::from(path), value);
        }
        Some((head, rest)) => {
            let slot = map
                .entry(Value::from(head))
                .or_insert_with(|| Value::Mapping(Mapping::new()));
            if !slot.is_mapping() {
                *slot = Value::Mapping(Mapping::new());
            }
            if let Value::Mapping(inner) = slot {
                set_path(inner, rest, value);
            }
        }
    }
}

/// Removes the value at a dotted `path`, and a parent mapping that is left empty.
fn remove_path(map: &mut Mapping, path: &str) {
    match path.split_once('.') {
        None => {
            map.remove(path);
        }
        Some((head, rest)) => {
            let emptied = match map.get_mut(head) {
                Some(Value::Mapping(inner)) => {
                    remove_path(inner, rest);
                    inner.is_empty()
                }
                _ => false,
            };
            if emptied {
                map.remove(head);
            }
        }
    }
}

/// The user config of the environment: `$XDG_CONFIG_HOME/rdy/config.yaml`, else
/// `$HOME/.config/rdy/config.yaml`.
pub fn user_file_from_env() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    user_file_of(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
}

/// The user config path rule over a given `XDG_CONFIG_HOME` and `HOME`.
pub(crate) fn user_file_of(xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match (xdg, home) {
        (Some(x), _) => Some(x.join("rdy/config.yaml")),
        (None, Some(h)) => Some(h.join(".config/rdy/config.yaml")),
        (None, None) => None,
    }
}

/// One level's file as the name a message uses for it.
pub(crate) fn file_name(level: Level, user_file: Option<&Path>) -> String {
    match level {
        Level::Local => LOCAL_FILE.to_string(),
        Level::Project => PROJECT_FILE.to_string(),
        Level::User | Level::Default => user_file.map_or_else(
            || "$XDG_CONFIG_HOME/rdy/config.yaml".to_string(),
            |f| f.display().to_string(),
        ),
    }
}

fn path_of(project: Option<&Path>, level: Level, user_file: Option<&Path>) -> Option<PathBuf> {
    match level {
        Level::Local => project.map(|p| p.join(LOCAL_FILE)),
        Level::Project => project.map(|p| p.join(PROJECT_FILE)),
        Level::User => user_file.map(Path::to_path_buf),
        Level::Default => None,
    }
}

/// A file read as a mapping of settings: `None` if it does not exist; an empty file is an empty
/// mapping. At most 1 MiB is read: a longer file is `E_LIMIT` before it is parsed.
pub(crate) fn read_file(path: &Path, shown: &str) -> Result<Option<Mapping>, RdyError> {
    use std::io::Read;
    let io_error = |e: std::io::Error| {
        RdyError::new(
            E_IO,
            format!("{shown}: {e}"),
            "check the file's permissions",
        )
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(crate::hub_config::config_limit(
            MAX_CONFIG_BYTES,
            LimitUnit::Bytes,
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        RdyError::new(
            E_IO,
            format!("{shown}: stream did not contain valid UTF-8"),
            "check the file's permissions",
        )
    })?;
    parse_file(&text, shown).map(Some)
}

/// A file's bytes, already read, as a mapping of settings (`read_file` without the read).
fn parse_bytes(bytes: &[u8], shown: &str) -> Result<Mapping, RdyError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(crate::hub_config::config_limit(
            MAX_CONFIG_BYTES,
            LimitUnit::Bytes,
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("{shown}: stream did not contain valid UTF-8 ({e})"),
            "check the file's permissions",
        )
    })?;
    parse_file(text, shown)
}

/// A file's text as a mapping of settings; an empty file is an empty mapping. The resource
/// bounds (size, depth, entries, scalar size, no alias) are checked before the parse. A syntax
/// error, a duplicate key, an alias or a key that is not a string names only the file: the
/// parser's text, which can quote a line, is never kept.
fn parse_file(text: &str, shown: &str) -> Result<Mapping, RdyError> {
    use crate::hub_config::settings_invalid;
    let value = crate::hub_config::parse_bounded(text)
        .map_err(|r| r.into_error(|| settings_invalid(Some(shown))))?;
    match value {
        Value::Null => Ok(Mapping::new()),
        Value::Mapping(m) if m.keys().all(Value::is_string) => Ok(m),
        Value::Mapping(_) => Err(settings_invalid(Some(shown))),
        _ => Err(RdyError::new(
            E_CONFIG,
            format!("{shown} must be a mapping of settings"),
            format!("fix {shown}"),
        )),
    }
}

/// The settings of the project at `project` with the user config at `user_file`; refuses a wrong
/// value type, a file that is not a mapping and a project that has no config. Of the `actor`
/// keys only the selected one is checked, as identity resolution checks it
/// ([`configured_actor`]): an invalid actor that a valid local one overrides is no error. A key
/// in a file of a level that does not allow it is a warning, and rdy ignores it.
pub fn effective(project: &Path, user_file: Option<&Path>) -> Result<Effective, RdyError> {
    build(Some(project), user_file, Read::Strict, None)
}

/// The settings as `rdy config` shows them: `effective`, except that the selected actor is only
/// type-checked. An actor outside the form is shown as written; resolving the person, and every
/// strict read, refuses it. It never creates a session.
pub fn inspect(project: &Path, user_file: Option<&Path>) -> Result<Effective, RdyError> {
    build(Some(project), user_file, Read::Inspect, None)
}

/// The bytes of the project config and of `.rdy/local.yaml` (`None` when it does not exist),
/// already read: `effective_from` parses them instead of reading the files again, so the
/// settings are exactly the bytes the pack fingerprint hashed.
pub struct GivenFiles<'a> {
    pub project: &'a [u8],
    pub local: Option<&'a [u8]>,
}

/// `effective` over the project and local files as `given` holds them; the user config is read
/// from `user_file`.
pub fn effective_from(
    project: &Path,
    user_file: Option<&Path>,
    given: GivenFiles<'_>,
) -> Result<Effective, RdyError> {
    build(Some(project), user_file, Read::Strict, Some(given))
}

/// `effective` that never fails: a wrong value type, or a file that does not read, is left out,
/// so a client can still start a daemon that then reports the error.
pub fn effective_lenient(project: &Path, user_file: Option<&Path>) -> Effective {
    build(Some(project), user_file, Read::Lenient, None).unwrap_or_else(|_| Effective {
        origins: Vec::new(),
        merged: Mapping::new(),
        warnings: Vec::new(),
    })
}

/// The `W_CONFIG_UNKNOWN` warnings of the project at `project`; never fails (a file that does not
/// read adds none).
pub fn config_warnings(project: &Path, user_file: Option<&Path>) -> Vec<Issue> {
    effective_lenient(project, user_file).warnings
}

/// `config_warnings` over the environment's user config, then [`pack_warnings`].
pub fn config_warnings_for_env(project: &Path) -> Vec<Issue> {
    let mut warnings = config_warnings(project, user_file_from_env().as_deref());
    warnings.extend(pack_warnings(project));
    warnings
}

/// The `W_PACK_SEVERITY` warnings of the project's pack (`pack_load::pack_severity_warnings`);
/// never fails (a pack that fails to load adds none).
pub fn pack_warnings(project: &Path) -> Vec<Issue> {
    crate::pack_load::project_pack(project)
        .map(|pack| crate::pack_load::pack_severity_warnings(&pack))
        .unwrap_or_default()
}

/// The warnings of the user config at `user_file` alone, for a command that runs with no
/// project (hub sign-in): every check `config_warnings` makes on that file, the same bounded
/// read and passes, and none that needs a project (its files and its code repos). Never fails:
/// a file that does not read adds none.
pub fn user_config_warnings(user_file: &Path) -> Vec<Issue> {
    build(None, Some(user_file), Read::Lenient, None)
        .map(|e| e.warnings)
        .unwrap_or_default()
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Local => "local",
        Level::Project => "project",
        Level::User => "user",
        Level::Default => "default",
    }
}

/// A path of a file that is neither a key of the table nor a section on the way to one.
struct Unknown {
    /// The dotted path (`decision.home`).
    path: String,
    /// The value, when the path ends in one mapping key that holds a `.` (`"serve.idle_timeout":
    /// 5`): rdy reads only nested keys, so that key is never known.
    dotted: Option<Value>,
}

/// The unknown paths of `map`. An unknown section is named by its leaves (`decision.home`), so a
/// typo in a section's name still gets the nearest key. A mapping key that holds a `.` is never a
/// key or a section of the table, whatever it names.
fn unknown_paths(map: &Mapping, prefix: &str, out: &mut Vec<Unknown>) {
    for (k, v) in map {
        let Some(k) = k.as_str() else { continue };
        let path = format!("{prefix}{k}");
        if k.contains('.') {
            out.push(Unknown {
                path,
                dotted: Some(v.clone()),
            });
            continue;
        }
        if KEY_LEVELS.iter().any(|r| r.key == path) {
            continue;
        }
        let dotted = format!("{path}.");
        if KEY_LEVELS.iter().any(|r| r.key.starts_with(&dotted)) {
            if let Value::Mapping(inner) = v {
                unknown_paths(inner, &dotted, out);
            }
            continue;
        }
        match v {
            Value::Mapping(inner) if !inner.is_empty() => unknown_paths(inner, &dotted, out),
            _ => out.push(Unknown { path, dotted: None }),
        }
    }
}

/// The fix of a dotted key: the same setting written nested, one segment a line. `home` names the
/// file to write it in when it is not the file it stands in (a review setting is project-only).
fn nested_fix(path: &str, value: &Value, home: &str) -> String {
    let segments: Vec<&str> = path.split('.').collect();
    let last = segments.len() - 1;
    let lines: Vec<String> = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let pad = "  ".repeat(i);
            if i == last {
                format!("`{pad}{seg}: {}`", flow(value))
            } else {
                format!("`{pad}{seg}:`")
            }
        })
        .collect();
    let next = if segments.len() > 2 {
        "on the next lines"
    } else {
        "on the next line"
    };
    format!("write it nested{home}: {} {next}", lines.join(" then "))
}

/// The rule of the table whose key is nearest to `path` by the closed did-you-mean rule, over
/// every key of the table.
fn nearest_key(path: &str) -> Option<&'static KeyRule> {
    let keys: Vec<&'static str> = KEY_LEVELS.iter().map(|r| r.key).collect();
    let k = crate::did_you_mean::nearest(path, &keys)?;
    KEY_LEVELS.iter().find(|r| r.key == k)
}

/// The files that may set a key, as a message names them: the project config, `.rdy/local.yaml`
/// and the user config, joined with ` or `.
fn home_name(levels: &[Level], user_file: Option<&Path>) -> String {
    [Level::Project, Level::Local, Level::User]
        .into_iter()
        .filter(|l| levels.contains(l))
        .map(|l| match l {
            Level::Project => PROJECT_FILE.to_string(),
            Level::Local => LOCAL_FILE.to_string(),
            _ => user_file.map_or_else(
                || "the user config".to_string(),
                |f| format!("the user config ({})", f.display()),
            ),
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The fix text of an unknown key: the nearest known key, else remove it. A nearest key that
/// `level` may not set is in another file, so the fix names it.
fn unknown_fix(path: &str, level: Level, file: &str, user_file: Option<&Path>) -> String {
    match nearest_key(path) {
        Some(r) if r.levels.contains(&level) => {
            format!("did you mean {}? rename {path} in {file}", r.key)
        }
        Some(r) => format!(
            "did you mean {}? set it in {} and remove {path} from {file}",
            r.key,
            home_name(r.levels, user_file)
        ),
        None => format!("remove {path} from {file}, or check its spelling"),
    }
}

/// The `W_CONFIG_UNKNOWN` warnings of `map`, one per unknown path. A key meant as a review
/// setting gets the review texts (#832): the review gate would stay off. That is a dotted key under
/// `reviews`, an unknown key under `reviews`, and one nearest to a `reviews` key. Founder ruling
/// (config typos warn): a warning, never `E_CONFIG`, so the project opens.
fn unknown_warnings(level: Level, map: &Mapping, user_file: Option<&Path>) -> Vec<Issue> {
    let mut paths = Vec::new();
    unknown_paths(map, "", &mut paths);
    let file = file_name(level, user_file);
    paths
        .into_iter()
        .map(|u| {
            let (message, fix) = unknown_text(level, &file, user_file, &u);
            config_warning(message, fix)
        })
        .collect()
}

fn config_warning(message: String, fix: String) -> Issue {
    Issue {
        severity: Severity::Warning,
        code: "W_CONFIG_UNKNOWN",
        block: None,
        message,
        fix,
    }
}

/// The message and fix of unknown path `u` in `file` (`level`): the review texts for a key meant
/// as a review setting, else the plain ones.
fn unknown_text(
    level: Level,
    file: &str,
    user_file: Option<&Path>,
    u: &Unknown,
) -> (String, String) {
    let path = &u.path;
    // review settings are project-only: a fix in another file names the project config
    let home = if level == Level::Project {
        String::new()
    } else {
        format!(" in {PROJECT_FILE}")
    };
    if let (Some(v), true) = (&u.dotted, path.starts_with("reviews.")) {
        return (
            format!(
                "{file}: `{path}` is written as one dotted key; rdy reads only nested keys, so \
                 the review setting would be ignored"
            ),
            nested_fix(path, v, &home),
        );
    }
    if path.starts_with("reviews.")
        || nearest_key(path).is_some_and(|r| r.key.starts_with("reviews."))
    {
        let consequence = if nearest_key(path).is_some_and(|r| r.key == "reviews.lease_ttl") {
            "leases keep the default of 10 minutes"
        } else {
            "the review gate would stay off"
        };
        return (
            format!("unknown review setting {path} in {file}; {consequence}"),
            unknown_fix(path, level, file, user_file),
        );
    }
    (
        format!(
            "unknown key {path} in {file} ({} level); rdy ignores it",
            level_name(level)
        ),
        match &u.dotted {
            Some(v) => nested_fix(path, v, ""),
            None => unknown_fix(path, level, file, user_file),
        },
    )
}

/// Whether some key under section `head` is allowed at `level`. A scalar in that section is then
/// a wrong type for that key (`E_CONFIG`), not a section that belongs in another file.
fn section_allowed_at(head: &str, level: Level) -> bool {
    KEY_LEVELS.iter().any(|r| {
        r.key
            .strip_prefix(head)
            .is_some_and(|rest| rest.starts_with('.'))
            && r.levels.contains(&level)
    })
}

/// The `W_CONFIG_UNKNOWN` warnings of the known keys that `map` (the file of `level`) holds and
/// `level` may not set, each key removed from `map` so that rdy ignores it. A project-only key's
/// section that is not a mapping counts too (`reviews: 5`); an empty section does not.
fn wrong_file_warnings(level: Level, map: &mut Mapping, user_file: Option<&Path>) -> Vec<Issue> {
    let file = file_name(level, user_file);
    let mut out = Vec::new();
    for rule in KEY_LEVELS {
        if rule.levels.contains(&level) {
            continue;
        }
        let head = rule.key.split('.').next().unwrap_or(rule.key);
        // the key to name: the whole key, or the section when it holds a scalar or a list
        let named = if get_path(map, rule.key).is_some() {
            remove_path(map, rule.key);
            rule.key
        } else if rule.key != head
            && map
                .get(head)
                .is_some_and(|v| !v.is_mapping() && !v.is_null())
            && !section_allowed_at(head, level)
        {
            map.remove(head);
            if out.iter().any(|(k, _): &(&str, Issue)| *k == head) {
                continue;
            }
            head
        } else {
            continue;
        };
        let home = home_name(rule.levels, user_file);
        let gate = if named == "reviews.lease_ttl" {
            ", so leases keep the default of 10 minutes"
        } else if named == "reviews" || named.starts_with("reviews.") {
            ", so the review gate would stay off"
        } else {
            ""
        };
        out.push((
            named,
            config_warning(
                format!("{named} in {file} belongs in {home}; rdy ignores it here{gate}"),
                format!("move {named} to {home}"),
            ),
        ));
    }
    out.into_iter().map(|(_, w)| w).collect()
}

/// The `W_CONFIG_UNKNOWN` warnings of the range-checked keys of `map`: a percent outside 0 to 100,
/// a port outside 1024 to 65535, an address that is not a private IPv4 literal, a count outside 1
/// to 1000, a choice that is not one of its words and a base path outside the closed form. rdy
/// keeps going (config mistakes warn). A percent, a port and an address are removed from `map`,
/// so the next level or the default applies. A count, a choice and a base path get the key's
/// default at the level that holds the wrong value: the message says "using <default>", so a
/// lower level never answers instead (FW-PC1, D-94BT2).
fn range_warnings(map: &mut Mapping) -> Vec<Issue> {
    let mut out = Vec::new();
    for rule in KEY_LEVELS {
        let (message, fix) = match rule.ty {
            Ty::Percent => {
                let outside = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::Number(n) => n.as_i64().is_none_or(|i| !(0..=100).contains(&i)),
                    _ => false,
                });
                if !outside {
                    continue;
                }
                (
                    format!(
                        "{} must be 0–100; using {}",
                        rule.key,
                        rule.default.unwrap_or("0")
                    ),
                    format!(
                        "set {} to a whole number from 0 to 100 in {PROJECT_FILE}",
                        rule.key
                    ),
                )
            }
            Ty::Port => {
                let outside = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::Number(n) => n.as_i64().is_none_or(|i| !(1024..=65535).contains(&i)),
                    _ => false,
                });
                if !outside {
                    continue;
                }
                (
                    format!(
                        "{} must be 1024–65535; using a port derived from the project path",
                        rule.key
                    ),
                    format!(
                        "set {} to a whole number from 1024 to 65535 in {LOCAL_FILE}",
                        rule.key
                    ),
                )
            }
            Ty::LanAddress => {
                let bad = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::String(s) => !s.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_private()),
                    _ => false,
                });
                if !bad {
                    continue;
                }
                (
                    format!(
                        "{} must be a private IPv4 address of this machine; using the default route's address",
                        rule.key
                    ),
                    format!(
                        "set {} to a private IPv4 address of this machine in {LOCAL_FILE}, or remove it",
                        rule.key
                    ),
                )
            }
            Ty::Count => {
                let outside = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::Number(n) => n.as_i64().is_none_or(|i| !(1..=1000).contains(&i)),
                    _ => false,
                });
                if !outside {
                    continue;
                }
                (
                    format!(
                        "{} must be 1–1000; using {}",
                        rule.key,
                        rule.default.unwrap_or("1")
                    ),
                    format!("set {} to a whole number from 1 to 1000", rule.key),
                )
            }
            Ty::Choice(choices) => {
                let outside = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::String(s) => !choices.contains(&s.as_str()),
                    _ => false,
                });
                if !outside {
                    continue;
                }
                let list = choice_list(choices);
                (
                    format!(
                        "{} must be {list}; using {}",
                        rule.key,
                        rule.default.unwrap_or("the default")
                    ),
                    format!("set {} to {list}", rule.key),
                )
            }
            Ty::BasePath => {
                let bad = get_path(map, rule.key).is_some_and(|v| match v {
                    Value::String(s) => !valid_site_base_path(s),
                    _ => false,
                });
                if !bad {
                    continue;
                }
                (
                    format!(
                        "{} must be / or an absolute path that ends with /, at most 2048 bytes, \
                         with only letters, digits, -, _, . and ~ between the slashes; using {}",
                        rule.key,
                        rule.default.unwrap_or("/")
                    ),
                    format!(
                        "set {} to / or to an absolute path that ends with /, such as /docs/, in \
                         {PROJECT_FILE}",
                        rule.key
                    ),
                )
            }
            _ => continue,
        };
        let default: Option<Value> = match rule.ty {
            Ty::Count | Ty::Choice(_) | Ty::BasePath => {
                rule.default.and_then(|d| serde_yaml_ng::from_str(d).ok())
            }
            _ => None,
        };
        match default {
            Some(d) => set_path(map, rule.key, d),
            None => remove_path(map, rule.key),
        }
        out.push(config_warning(message, fix));
    }
    out
}

/// The warning for a user-config `signing.allowed_signers` that is a relative path, the value
/// removed from `map` so that no level answers instead. The path is used as written: no tilde,
/// environment variable or glob is expanded. An empty string stays for the signing path's own
/// error.
fn user_path_warnings(map: &mut Mapping, user_file: Option<&Path>) -> Vec<Issue> {
    let relative = get_path(map, SIGNING_ALLOWED_SIGNERS)
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty() && !Path::new(s).is_absolute());
    if !relative {
        return Vec::new();
    }
    remove_path(map, SIGNING_ALLOWED_SIGNERS);
    vec![config_warning(
        format!(
            "{SIGNING_ALLOWED_SIGNERS} in {} must be absolute; rdy ignores it",
            file_name(Level::User, user_file)
        ),
        format!("set {SIGNING_ALLOWED_SIGNERS} to an absolute path"),
    )]
}

/// The warning for a winning `signing.agent_key` that names the same key as the winning
/// `signing.key`, from the merged settings. The agent value is removed from `merged`, so no lower
/// level answers instead; its origin stays for `--show-origin`. The selectors are compared by
/// fingerprint only: a malformed one is left for the signing path's error, and no agent or file
/// is asked.
fn signing_agent_warning(merged: &mut Mapping) -> Option<Issue> {
    let key = get_path(merged, SIGNING_KEY)?.as_str()?;
    let agent = get_path(merged, SIGNING_AGENT_KEY)?.as_str()?;
    let same = crate::signing_crypto::key_fingerprint(key)
        .ok()
        .zip(crate::signing_crypto::key_fingerprint(agent).ok())
        .is_some_and(|(a, b)| a == b);
    if !same {
        return None;
    }
    remove_path(merged, SIGNING_AGENT_KEY);
    Some(config_warning(
        format!("{SIGNING_AGENT_KEY} equals {SIGNING_KEY}; rdy ignores {SIGNING_AGENT_KEY}"),
        format!("set {SIGNING_AGENT_KEY} to a different key"),
    ))
}

/// The warning for `serve.site_lan_address` while `serve.site_lan` is not true, from the merged
/// settings. The value stays: it applies once the setting is on.
fn lan_address_warning(merged: &Mapping) -> Option<Issue> {
    get_path(merged, "serve.site_lan_address")?;
    let on = get_path(merged, "serve.site_lan").and_then(Value::as_bool) == Some(true);
    if on {
        return None;
    }
    Some(config_warning(
        "serve.site_lan_address has no effect while serve.site_lan is false".to_string(),
        format!("set serve.site_lan to true in {LOCAL_FILE}, or remove serve.site_lan_address"),
    ))
}

/// Why the value `rule.key` has in `map` is of the wrong type, or `None` when it has the type or
/// is not set. A parent section that is not a mapping is the wrong type too.
fn wrong_type(map: &Mapping, rule: &KeyRule) -> Option<String> {
    let mut cur = map;
    let mut parts = rule.key.split('.').peekable();
    while let Some(part) = parts.next() {
        let v = cur.get(part)?;
        if parts.peek().is_none() {
            return rule.ty.check(v).err();
        }
        match v {
            Value::Mapping(inner) => cur = inner,
            Value::Null => return None,
            other => {
                return Some(format!(
                    "{part} must be a mapping of settings, not {}",
                    flow(other)
                ));
            }
        }
    }
    None
}

/// How a read of the levels treats what it cannot use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Every error refuses; the selected actor is checked as identity resolution checks it.
    Strict,
    /// `Strict`, except that the selected actor is only type-checked (the config display).
    Inspect,
    /// Nothing refuses: a file that does not read, or a wrong type, is left out (warnings only;
    /// never a session).
    Lenient,
}

/// The settings of the levels: with no `project`, the user config alone (lenient only).
fn build(
    project: Option<&Path>,
    user_file: Option<&Path>,
    read: Read,
    given: Option<GivenFiles<'_>>,
) -> Result<Effective, RdyError> {
    let lenient = read == Read::Lenient;
    if let Some(p) = project
        && !lenient
        && given.is_none()
        && !p.join(PROJECT_FILE).exists()
    {
        return Err(missing_project(p));
    }
    // each file's mapping, in precedence order
    let mut files: Vec<(Level, Mapping)> = Vec::new();
    for level in FILE_LEVELS {
        let Some(path) = path_of(project, level, user_file) else {
            continue;
        };
        let shown = file_name(level, user_file);
        let read = match (&given, level) {
            (Some(g), Level::Project) => parse_bytes(g.project, &shown).map(Some),
            (Some(g), Level::Local) => g.local.map(|b| parse_bytes(b, &shown)).transpose(),
            _ => read_file(&path, &shown),
        };
        match read {
            Ok(Some(m)) => files.push((level, m)),
            Ok(None) => {}
            Err(e) if lenient => drop(e),
            Err(e) => return Err(e),
        }
    }
    // per file in precedence order: its unknown keys, then its keys that belong in another file
    // (removed here, so they never reach the type check)
    let mut warnings: Vec<Issue> = Vec::new();
    // the hub subtree and the remotes leave each file first: `hub_config` checks them, and no
    // value of theirs reaches the passes below (or a message of theirs)
    let mut hub_parts = Vec::new();
    let mut hub_notes = Vec::new();
    for (level, map) in &mut files {
        let (part, notes) =
            crate::hub_config::take_level(*level, map, &file_name(*level, user_file));
        hub_parts.push((*level, part));
        hub_notes.extend(notes);
    }
    let hub = crate::hub_config::merge(hub_parts, hub_notes, project, user_file);
    for (level, map) in &mut files {
        warnings.extend(unknown_warnings(*level, map, user_file));
        warnings.extend(wrong_file_warnings(*level, map, user_file));
    }
    warnings.extend(hub.warnings.iter().cloned());
    for (level, map) in &mut files {
        for rule in KEY_LEVELS {
            // a refusing read checks only the selected actor, below, as resolution does
            if rule.key == ACTOR_KEY && !lenient {
                continue;
            }
            let Some(why) = wrong_type(map, rule) else {
                continue;
            };
            if !lenient {
                let shown = file_name(*level, user_file);
                return Err(RdyError::new(
                    E_CONFIG,
                    format!("{} in {shown}: {why}", rule.key),
                    format!("fix {} in {shown}", rule.key),
                ));
            }
            remove_path(map, rule.key);
            if let Some((head, _)) = rule.key.split_once('.')
                && map.get(head).is_some_and(|v| !v.is_mapping())
            {
                map.remove(head);
            }
        }
    }
    if !lenient {
        select_actor(&mut files, user_file, read)?;
    }
    for (level, map) in &mut files {
        warnings.extend(range_warnings(map));
        if *level == Level::User {
            warnings.extend(user_path_warnings(map, user_file));
        }
    }
    let mut merged = files
        .iter()
        .find(|(l, _)| *l == Level::Project)
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    // O15: from the mappings parsed above, never a second read; a wrong type adds no warning
    // and no `code_paths` row here (the code briefing reports it)
    let map_of = |level: Level| files.iter().find(|(l, _)| *l == level).map(|(_, m)| m);
    // code repos are a project's: with no project there are none, and no warning of theirs
    let code = match project {
        Some(p) => linked_core(
            p,
            map_of(Level::Project),
            map_of(Level::Local),
            map_of(Level::User),
            user_file,
        ),
        None => Ok((Vec::new(), Vec::new())),
    };
    let mut origins = Vec::new();
    for rule in KEY_LEVELS {
        if rule.key == crate::hub_config::REMOTES_KEY {
            if let Some(v) = hub.remotes_value() {
                set_path(&mut merged, rule.key, v);
            }
            origins.extend(hub.remote_rows.iter().cloned());
            continue;
        }
        if rule.key == crate::hub_config::HUB_INSTALLS_KEY {
            if let Some(v) = hub.installs_value() {
                set_path(&mut merged, rule.key, v);
            }
            origins.extend(hub.install_rows.iter().cloned());
            continue;
        }
        if rule.key == CODE_PATHS_KEY {
            if let (Ok((linked, _)), Some(project)) = (&code, project) {
                let rows = code_path_origins(project, linked, map_of, user_file);
                if !rows.is_empty() {
                    let entries: Mapping = rows
                        .iter()
                        .map(|o| {
                            (
                                Value::from(&o.key[CODE_PATHS_KEY.len() + 1..]),
                                o.value.clone(),
                            )
                        })
                        .collect();
                    set_path(&mut merged, CODE_PATHS_KEY, Value::Mapping(entries));
                }
                origins.extend(rows);
            }
            continue;
        }
        let found = files
            .iter()
            .find_map(|(level, map)| get_path(map, rule.key).map(|v| (*level, v.clone())));
        if let Some((level, value)) = found {
            set_path(&mut merged, rule.key, value.clone());
            origins.push(Origin {
                key: rule.key.to_string(),
                value,
                level,
                file: path_of(project, level, user_file),
            });
            continue;
        }
        let default = match rule.key {
            // the bounded fallback the person resolves to (D-Q8NFG): human:unknown with no USER
            ACTOR_KEY => Some(Value::from(crate::identity::fallback_actor(
                std::env::var("USER").ok().as_deref(),
            ))),
            _ => rule.default.and_then(|d| serde_yaml_ng::from_str(d).ok()),
        };
        if let Some(value) = default {
            origins.push(Origin {
                key: rule.key.to_string(),
                value,
                level: Level::Default,
                file: None,
            });
        }
    }
    warnings.extend(lan_address_warning(&merged));
    warnings.extend(signing_agent_warning(&mut merged));
    if let Ok((_, code)) = code {
        warnings.extend(code);
    }
    Ok(Effective {
        origins,
        merged,
        warnings,
    })
}

/// One origin per linked repo (F1), sorted by name: key `code_paths.<name>`, the path as its file
/// writes it, and the level and file that supplied it.
fn code_path_origins<'a>(
    project: &Path,
    linked: &[LinkedRepo],
    map_of: impl Fn(Level) -> Option<&'a Mapping>,
    user_file: Option<&Path>,
) -> Vec<Origin> {
    let mut rows: Vec<Origin> = linked
        .iter()
        .filter_map(|l| {
            let value = map_of(l.level)?
                .get(CODE_PATHS_KEY)?
                .as_mapping()?
                .get(l.name.as_str())?
                .clone();
            Some(Origin {
                key: format!("{CODE_PATHS_KEY}.{}", l.name),
                value,
                level: l.level,
                file: path_of(Some(project), l.level, user_file),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

/// The key of `.rdy/local.yaml` and the user config that maps each code repo's name to the path
/// of its checkout (FW-X5).
pub const CODE_PATHS_KEY: &str = "code_paths";

/// Whether `s` is a code repo's name: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}` (O11).
pub fn is_repo_name(s: &str) -> bool {
    let b = s.as_bytes();
    match b.split_first() {
        Some((first, rest)) => {
            first.is_ascii_alphanumeric()
                && rest.len() <= 63
                && rest
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        }
        None => false,
    }
}

/// A linked repo before its checks: its name, its path made absolute (not canonical), and the
/// level that gave the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedRepo {
    pub name: String,
    pub path: PathBuf,
    pub level: Level,
}

/// The names of `code_repos` (`.rdy.yaml`), each with its path from `code_paths`: the local
/// entry, else the user entry (F1). The warnings are the closed list of [`linked_core`].
/// `E_CONFIG` only for a wrong YAML type: `code_repos` not a list, more than 16 entries, an entry
/// that is not a non-empty string; `code_paths` not a mapping, or an entry whose value is not a
/// string. A file that does not parse is `E_CONFIG` too, as in [`effective`]. A missing
/// `.rdy.yaml` is [`missing_project`]'s error.
pub fn linked_repos(
    project: &Path,
    user_file: Option<&Path>,
) -> Result<(Vec<LinkedRepo>, Vec<Issue>), RdyError> {
    let read = |level: Level| match path_of(Some(project), level, user_file) {
        Some(path) => read_file(&path, &file_name(level, user_file)),
        None => Ok(None),
    };
    // `.rdy.yaml` is mandatory: missing, it is the missing-project error, never no repos (an
    // engine already open reads it again here); the local and user files stay optional
    let project_map = read(Level::Project)?.ok_or_else(|| missing_project(project))?;
    let local = read(Level::Local)?;
    let user = read(Level::User)?;
    linked_core(
        project,
        Some(&project_map),
        local.as_ref(),
        user.as_ref(),
        user_file,
    )
}

/// The entries of `code_repos` in the project file, as written; no key and a null value are none.
fn repo_entries(project_map: Option<&Mapping>) -> Result<Vec<&str>, RdyError> {
    use crate::code_index::{CODE_REPOS_KEY, MAX_CODE_REPOS};
    let bad = || {
        RdyError::new(
            E_CONFIG,
            format!("{CODE_REPOS_KEY} must be a list of repo names (at most {MAX_CODE_REPOS})"),
            format!("fix {CODE_REPOS_KEY} in {PROJECT_FILE}, e.g. {CODE_REPOS_KEY}: [app]"),
        )
    };
    let value = match project_map.and_then(|m| m.get(CODE_REPOS_KEY)) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(v) => v,
    };
    let seq = value.as_sequence().ok_or_else(bad)?;
    if seq.len() > MAX_CODE_REPOS {
        return Err(bad());
    }
    seq.iter()
        .map(|e| e.as_str().filter(|s| !s.is_empty()).ok_or_else(bad))
        .collect()
}

/// The entries of `code_paths` in the file of `level`, in file order; no key is none. A value that
/// is not a mapping (a null included) is a wrong type, as a key or an entry's value that is not a
/// string is.
fn path_entries<'a>(
    map: Option<&'a Mapping>,
    level: Level,
    user_file: Option<&Path>,
) -> Result<Vec<(&'a str, &'a str)>, RdyError> {
    let bad = || {
        RdyError::new(
            E_CONFIG,
            "code_paths must map repo names to paths",
            format!(
                "fix {CODE_PATHS_KEY} in {}, e.g. {CODE_PATHS_KEY}: {{app: ../app}}",
                file_name(level, user_file)
            ),
        )
    };
    let Some(value) = map.and_then(|m| m.get(CODE_PATHS_KEY)) else {
        return Ok(Vec::new());
    };
    let entries = value.as_mapping().ok_or_else(bad)?;
    entries
        .iter()
        .map(|(k, v)| match (k.as_str(), v.as_str()) {
            (Some(name), Some(path)) => Ok((name, path)),
            _ => Err(bad()),
        })
        .collect()
}

/// [`linked_repos`] over parsed mappings: `linked_repos` reads the files, `build` passes the
/// mappings it already parsed (O15). The warnings, each `W_CONFIG_UNKNOWN`, are a closed list:
/// 1. an entry of `code_repos` that is not a name ([`is_repo_name`]; F2);
/// 2. a name listed again (once per name);
/// 3. a name with no `code_paths` entry at either level;
/// 4. a `code_paths` entry whose name `code_repos` does not list;
/// 5. a user config entry whose path is not absolute (rdy ignores it, and the name has no path
///    from that level).
///
/// Warnings 1 to 3 come in `code_repos` order, then 4 for `.rdy/local.yaml`, then 4 and 5 for the
/// user config, each in file order. A local path that is relative starts at the project root.
fn linked_core(
    project: &Path,
    project_map: Option<&Mapping>,
    local: Option<&Mapping>,
    user: Option<&Mapping>,
    user_file: Option<&Path>,
) -> Result<(Vec<LinkedRepo>, Vec<Issue>), RdyError> {
    let entries = repo_entries(project_map)?;
    let local_paths = path_entries(local, Level::Local, user_file)?;
    let user_paths = path_entries(user, Level::User, user_file)?;
    let mut warnings = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    let mut repeated: Vec<&str> = Vec::new();
    let mut linked = Vec::new();
    for entry in entries {
        if !is_repo_name(entry) {
            warnings.push(config_warning(
                format!("code_repos entry {entry} is not a repo name, so rdy ignores it"),
                format!(
                    "name the repo in code_repos and give its path under {CODE_PATHS_KEY} in \
                     {LOCAL_FILE}"
                ),
            ));
            continue;
        }
        if listed.contains(&entry) {
            if !repeated.contains(&entry) {
                repeated.push(entry);
                warnings.push(config_warning(
                    format!("code_repos lists {entry} twice; rdy uses it once"),
                    format!("remove the second {entry} from code_repos in {PROJECT_FILE}"),
                ));
            }
            continue;
        }
        listed.push(entry);
        let in_local = local_paths.iter().find(|(n, _)| *n == entry);
        let in_user = user_paths.iter().find(|(n, _)| *n == entry);
        match (in_local, in_user) {
            (Some((_, path)), _) => {
                let joined = project.join(path);
                linked.push(LinkedRepo {
                    name: entry.to_string(),
                    path: std::path::absolute(&joined).unwrap_or(joined),
                    level: Level::Local,
                });
            }
            (None, Some((_, path))) if Path::new(path).is_absolute() => {
                linked.push(LinkedRepo {
                    name: entry.to_string(),
                    path: PathBuf::from(path),
                    level: Level::User,
                });
            }
            // a relative user path: warning 5, in the user config's pass below
            (None, Some(_)) => {}
            (None, None) => warnings.push(config_warning(
                format!("code repo {entry} has no checkout path, so rdy skips it"),
                format!(
                    "add {CODE_PATHS_KEY}: {{{entry}: <path to its checkout>}} to {LOCAL_FILE} or \
                     the user config"
                ),
            )),
        }
    }
    let unlisted = |name: &str| {
        config_warning(
            format!(
                "{CODE_PATHS_KEY} names {name}, which code_repos does not list, so rdy ignores it"
            ),
            format!(
                "add {name} to code_repos in {PROJECT_FILE}, or remove it from {CODE_PATHS_KEY}"
            ),
        )
    };
    for (name, _) in &local_paths {
        if !listed.contains(name) {
            warnings.push(unlisted(name));
        }
    }
    for (name, path) in &user_paths {
        if !listed.contains(name) {
            warnings.push(unlisted(name));
        } else if !Path::new(path).is_absolute() {
            warnings.push(config_warning(
                format!(
                    "{CODE_PATHS_KEY}.{name} in {} is not an absolute path, so rdy ignores it",
                    file_name(Level::User, user_file)
                ),
                format!("give an absolute path there, or set it in {LOCAL_FILE}"),
            ));
        }
    }
    Ok((linked, warnings))
}

/// The actor forms a configured actor may take, as a fix names them.
const ACTOR_FORMS: &str =
    "human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";

/// The key of the configured person.
const ACTOR_KEY: &str = "actor";

/// The actor the shared selector picks (D-Q8NFG, O5, O9): of the raw `actor` values of
/// `.rdy/local.yaml` and the user config, each with the name of its file, the local one if
/// present, else the user's, with that name. `None` only when neither file has the key; a
/// present key is never skipped. The selected value must be a string: any other value, a YAML
/// null among them, is `E_CONFIG` in the words of the effective read's type check, never absent
/// and never an empty actor.
fn selected_actor<'a>(
    local: Option<(&'a Value, &'a str)>,
    user: Option<(&'a Value, &'a str)>,
) -> Result<Option<(&'a str, &'a str)>, RdyError> {
    let Some((value, shown)) = local.or(user) else {
        return Ok(None);
    };
    let rule = KEY_LEVELS
        .iter()
        .find(|r| r.key == ACTOR_KEY)
        .expect("actor is a settable key");
    let type_error = |why: String| {
        RdyError::new(
            E_CONFIG,
            format!("{} in {shown}: {why}", rule.key),
            format!("fix {} in {shown}", rule.key),
        )
    };
    rule.ty.check(value).map_err(type_error)?;
    match value.as_str() {
        Some(actor) => Ok(Some((actor, shown))),
        None => Err(type_error(format!(
            "invalid type: {}, expected a string",
            flow(value)
        ))),
    }
}

/// The configured person (D-Q8NFG, O5, O9): the actor [`selected_actor`] picks, `None` only when
/// neither file has the key. A string outside the actor form (an empty one among them) is the
/// grammar's `E_BAD_INPUT` with a fix naming the file. The value is never rewritten.
pub(crate) fn configured_actor(
    local: Option<(&Value, &str)>,
    user: Option<(&Value, &str)>,
) -> Result<Option<String>, RdyError> {
    let Some((actor, shown)) = selected_actor(local, user)? else {
        return Ok(None);
    };
    crate::identity::validate_actor(actor).map_err(|mut e| {
        e.fix = format!("set actor in {shown} to {ACTOR_FORMS}");
        e
    })?;
    Ok(Some(actor.to_string()))
}

/// The selected actor of the read files, checked as `read` checks it (`Strict`: as identity
/// resolution, [`configured_actor`]; `Inspect`: its type alone); the user config's actor, when a
/// local one overrides it, is dropped unchecked, so no value the selector ignores reaches the
/// settings.
fn select_actor(
    files: &mut [(Level, Mapping)],
    user_file: Option<&Path>,
    read: Read,
) -> Result<(), RdyError> {
    let local_shown = file_name(Level::Local, user_file);
    let user_shown = file_name(Level::User, user_file);
    let map_of = |level: Level| files.iter().find(|(l, _)| *l == level).map(|(_, m)| m);
    let local = actor_value(map_of(Level::Local), &local_shown);
    let user = actor_value(map_of(Level::User), &user_shown);
    let overridden = local.is_some() && user.is_some();
    match read {
        Read::Strict => {
            configured_actor(local, user)?;
        }
        Read::Inspect => {
            selected_actor(local, user)?;
        }
        Read::Lenient => {}
    }
    if overridden && let Some((_, map)) = files.iter_mut().find(|(l, _)| *l == Level::User) {
        map.remove(ACTOR_KEY);
    }
    Ok(())
}

/// The raw `actor` value of a file read as a mapping, with the file's name.
fn actor_value<'a>(map: Option<&'a Mapping>, shown: &'a str) -> Option<(&'a Value, &'a str)> {
    map.and_then(|m| m.get(ACTOR_KEY)).map(|v| (v, shown))
}

/// The human actor of the user config at `user_file` alone: its `actor`, read within the config
/// bounds, else the bounded fallback of `user`.
pub(crate) fn user_actor(user_file: Option<&Path>, user: Option<&str>) -> Result<String, RdyError> {
    let shown = file_name(Level::User, user_file);
    let map = match user_file {
        Some(f) => read_file(f, &shown)?,
        None => None,
    };
    Ok(configured_actor(None, actor_value(map.as_ref(), &shown))?
        .unwrap_or_else(|| crate::identity::fallback_actor(user)))
}

/// The human actor of the project: `actor` in `.rdy/local.yaml`, else in the user config, else
/// the bounded fallback of `user` (D-Q8NFG). Both files are read within the config bounds, and a
/// file that does not read is its error even when the other names a valid actor; the selected
/// value is checked by [`configured_actor`].
pub fn human_actor_for(
    project: &Path,
    user_file: Option<&Path>,
    user: Option<&str>,
) -> Result<String, RdyError> {
    let local_shown = file_name(Level::Local, user_file);
    let local = read_file(&project.join(LOCAL_FILE), &local_shown)?;
    let user_shown = file_name(Level::User, user_file);
    let user_map = match user_file {
        Some(f) => read_file(f, &user_shown)?,
        None => None,
    };
    Ok(configured_actor(
        actor_value(local.as_ref(), &local_shown),
        actor_value(user_map.as_ref(), &user_shown),
    )?
    .unwrap_or_else(|| crate::identity::fallback_actor(user)))
}

/// `human_actor_for` over `XDG_CONFIG_HOME`, `HOME` and `USER`: the one place product code reads
/// them.
pub fn human_actor_for_env(project: &Path) -> Result<String, RdyError> {
    let user = std::env::var("USER").ok();
    human_actor_for(project, user_file_from_env().as_deref(), user.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(config: &str, local: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".rdy")).unwrap();
        std::fs::write(d.path().join(PROJECT_FILE), config).unwrap();
        if let Some(l) = local {
            std::fs::write(d.path().join(LOCAL_FILE), l).unwrap();
        }
        d
    }

    #[test]
    fn cfgl1_lenient_drops_a_wrong_level_key_and_keeps_the_rest() {
        let d = project(
            "pack: p\nactor: human:all\nserve:\n  idle_timeout: 5\n",
            Some("pack: x\n"),
        );
        let e = effective_lenient(d.path(), None);
        assert!(
            e.origins
                .iter()
                .all(|o| o.key != "actor" || o.level == Level::Default),
            "{:?}",
            e.origins
        );
        let pack = e.origins.iter().find(|o| o.key == "pack").unwrap();
        assert_eq!(
            (pack.level, pack.value.as_str()),
            (Level::Project, Some("p"))
        );
        assert!(e.merged.get("actor").is_none());
        assert_eq!(
            get_path(&e.merged, "serve.idle_timeout"),
            Some(&Value::from(5u64))
        );
    }

    #[test]
    fn cfgl1_keys_outside_the_table_are_kept_and_never_shown() {
        let d = project("pack: p\nlater: 1\n", None);
        let e = effective(d.path(), None).unwrap();
        assert_eq!(e.merged.get("later"), Some(&Value::from(1u64)));
        assert!(e.origins.iter().all(|o| o.key != "later"));
    }

    #[test]
    fn cfgl1_a_wrong_typed_value_is_e_config_naming_its_file_and_key() {
        let d = project("pack: p\n", Some("serve:\n  idle_timeout: soon\n"));
        let e = effective(d.path(), None).unwrap_err();
        assert_eq!(e.code, E_CONFIG);
        assert!(
            e.message
                .starts_with("serve.idle_timeout in .rdy/local.yaml: ")
                && e.message.contains("soon"),
            "{e}"
        );
        assert_eq!(e.fix, "fix serve.idle_timeout in .rdy/local.yaml");
        // the user config, the project config and a section that is not a mapping
        let u = tempfile::tempdir().unwrap();
        let uf = u.path().join("config.yaml");
        std::fs::write(&uf, "views:\n  idle_days: many\n").unwrap();
        let e = effective(project("pack: p\n", None).path(), Some(&uf)).unwrap_err();
        assert!(
            e.message
                .starts_with(&format!("views.idle_days in {}: ", uf.display())),
            "{e}"
        );
        assert!(e.fix.contains(&uf.display().to_string()), "{e}");
        let d = project("pack: p\nreviews:\n  require_up_to_date: maybe\n", None);
        let e = effective(d.path(), None).unwrap_err();
        assert!(
            e.message
                .starts_with("reviews.require_up_to_date in .rdy.yaml: "),
            "{e}"
        );
        let d = project("pack: p\nserve: 5\n", None);
        let e = effective(d.path(), None).unwrap_err();
        assert!(
            e.message.starts_with("serve.start_timeout in .rdy.yaml: "),
            "{e}"
        );
        // an empty optional value and an empty section are fine
        let d = project("pack: p\nname:\nserve:\n  idle_timeout:\n", None);
        assert!(effective(d.path(), None).is_ok());
    }

    #[test]
    fn cfgl1_lenient_drops_a_wrong_typed_value_and_a_bad_section() {
        let d = project(
            "pack: p\nserve: 5\nviews:\n  idle_days: 3\n",
            Some("serve:\n  idle_timeout: soon\n"),
        );
        let e = effective_lenient(d.path(), None);
        assert!(e.merged.get("serve").is_none());
        assert_eq!(
            get_path(&e.merged, "views.idle_days"),
            Some(&Value::from(3u64))
        );
    }

    #[test]
    fn cfgw1_an_unknown_key_at_project_level_warns_and_names_it() {
        let d = project("pack: p\nlater: 1\n", None);
        let e = effective(d.path(), None).unwrap();
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        let w = &e.warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN");
        assert_eq!(w.severity, rdy_schema::Severity::Warning);
        assert!(
            w.message.contains("later")
                && w.message.contains(".rdy.yaml")
                && w.message.contains("project"),
            "{}",
            w.message
        );
        assert!(!w.fix.contains("did you mean"), "{}", w.fix);
        // the lenient read carries it too, and a nested unknown key is named by its path
        let d = project("pack: p\nserve:\n  idle_timout: 5\n", None);
        let e = effective_lenient(d.path(), None);
        assert!(e.warnings[0].message.contains("serve.idle_timout"));
        assert!(
            e.warnings[0].fix.contains("serve.idle_timeout"),
            "{}",
            e.warnings[0].fix
        );
    }

    /// Founder ruling (config typos warn): an unknown review setting is a warning in every
    /// read, with the review-specific text; the project opens.
    #[test]
    fn msw19_an_unknown_review_setting_is_a_warning_in_every_read() {
        let d = project("pack: p\nreviews:\n  require_uptodate: true\n", None);
        let strict = effective(d.path(), None).unwrap().warnings;
        let lenient = effective_lenient(d.path(), None).warnings;
        for w in [&strict, &lenient] {
            assert_eq!(w.len(), 1, "{w:?}");
            assert_eq!(w[0].code, "W_CONFIG_UNKNOWN");
            assert_eq!(
                w[0].message,
                "unknown review setting reviews.require_uptodate in .rdy.yaml; the review gate would stay off"
            );
            assert!(
                w[0].fix
                    .contains("did you mean reviews.require_up_to_date?"),
                "{}",
                w[0].fix
            );
        }
        // an unknown key outside reviews stays a plain warning
        let d = project("pack: p\nlater: 1\n", None);
        assert_eq!(effective(d.path(), None).unwrap().warnings.len(), 1);
    }

    /// M14: a dotted key of three segments, or one inside a section, is nested a segment a line.
    #[test]
    fn msw19_a_dotted_key_of_three_segments_nests_each_segment() {
        for config in ["pack: p\nserve.a.b: 1\n", "pack: p\nserve:\n  a.b: 1\n"] {
            let d = project(config, None);
            let w = effective(d.path(), None).unwrap().warnings;
            assert_eq!(w.len(), 1, "{config:?}: {w:?}");
            assert_eq!(
                w[0].fix, "write it nested: `serve:` then `  a:` then `    b: 1` on the next lines",
                "{config:?}"
            );
        }
    }

    /// M14: review settings are project-only, so a review key in .rdy/local.yaml is sent to the
    /// project config.
    #[test]
    fn msw19_a_review_setting_in_local_yaml_names_the_project_config() {
        let d = project("pack: p\n", Some("require_up_to_date: true\n"));
        let w = effective(d.path(), None).unwrap().warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            w[0].message,
            "unknown key require_up_to_date in .rdy/local.yaml (local level); rdy ignores it"
        );
        assert_eq!(
            w[0].fix,
            "remove require_up_to_date from .rdy/local.yaml, or check its spelling"
        );
        let d = project("pack: p\n", Some("reviews.require_up_to_date: true\n"));
        let w = effective(d.path(), None).unwrap().warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            w[0].fix,
            "write it nested in .rdy.yaml: `reviews:` then `  require_up_to_date: true` on the next line"
        );
    }

    #[test]
    fn cfgw1_decision_home_suggests_decisions_home() {
        let d = project("pack: p\ndecision:\n  home: docs\n", None);
        let e = effective(d.path(), None).unwrap();
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        assert!(e.warnings[0].message.contains("decision.home"));
        assert!(
            e.warnings[0].fix.contains("decisions.home"),
            "{}",
            e.warnings[0].fix
        );
    }

    #[test]
    fn cfgw1_a_known_key_at_the_wrong_level_warns_naming_the_right_file() {
        let d = project("pack: p\n", Some("pack: x\n"));
        let e = effective(d.path(), None).expect("a wrong-file key is a warning");
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert_eq!(e.warnings[0].fix, "move pack to .rdy.yaml");
        let d = project("pack: p\n", Some("decisions:\n  home: x\n"));
        let e = effective(d.path(), None).expect("a wrong-file key is a warning");
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        assert_eq!(e.warnings[0].fix, "move decisions.home to .rdy.yaml");
    }

    #[test]
    fn cfgw1_known_keys_give_no_warning() {
        let d = project(
            "pack: p\nname: n\ndecisions:\n  home: [a, b]\ncode_repos:\n  x: y\nserve:\n  idle_timeout: 5\n",
            Some("actor: human:a\nviews:\n  idle_days: 3\n"),
        );
        let e = effective(d.path(), None).unwrap();
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        assert!(config_warnings(d.path(), None).is_empty());
    }

    #[test]
    fn cfgw1_an_unknown_key_in_the_user_file_names_the_user_level() {
        let d = project("pack: p\n", None);
        let u = tempfile::tempdir().unwrap();
        let uf = u.path().join("config.yaml");
        std::fs::write(&uf, "actr: human:x\n").unwrap();
        let w = config_warnings(d.path(), Some(&uf));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].message.contains("actr") && w[0].message.contains("user"),
            "{}",
            w[0].message
        );
        assert!(w[0].fix.contains("did you mean actor"), "{}", w[0].fix);
    }

    #[test]
    fn hd9_a_review_setting_in_local_yaml_warns_naming_the_project_config_and_is_ignored() {
        let d = project("pack: p\n", Some("reviews:\n  require_up_to_date: true\n"));
        let e = effective(d.path(), None).expect("a wrong-file key is a warning");
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        let w = &e.warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            w.message,
            "reviews.require_up_to_date in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here, so the review gate would stay off"
        );
        assert_eq!(w.fix, "move reviews.require_up_to_date to .rdy.yaml");
        let o = e
            .origins
            .iter()
            .find(|o| o.key == "reviews.require_up_to_date")
            .unwrap();
        assert_eq!(
            (o.level, o.value.as_bool()),
            (Level::Default, Some(false)),
            "not applied"
        );
        assert_eq!(
            effective_lenient(d.path(), None).warnings,
            e.warnings,
            "every read warns"
        );
    }

    #[test]
    fn hd9_a_review_setting_in_the_user_config_warns_naming_the_project_config() {
        let d = project("pack: p\n", None);
        let u = tempfile::tempdir().unwrap();
        let uf = u.path().join("config.yaml");
        std::fs::write(&uf, "reviews:\n  require_up_to_date: true\n").unwrap();
        let e = effective(d.path(), Some(&uf)).expect("a wrong-file key is a warning");
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        assert!(
            e.warnings[0].message.contains(&uf.display().to_string()),
            "{}",
            e.warnings[0].message
        );
        assert_eq!(
            e.warnings[0].fix,
            "move reviews.require_up_to_date to .rdy.yaml"
        );
    }

    #[test]
    fn hd9_a_misspelt_review_key_in_local_yaml_names_the_project_config() {
        let d = project("pack: p\n", Some("reviews:\n  require_uptodate: true\n"));
        let w = effective(d.path(), None).unwrap().warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            w[0].message,
            "unknown review setting reviews.require_uptodate in .rdy/local.yaml; the review gate would stay off"
        );
        assert_eq!(
            w[0].fix,
            "did you mean reviews.require_up_to_date? set it in .rdy.yaml and remove reviews.require_uptodate from .rdy/local.yaml"
        );
    }

    #[test]
    fn hd9_actor_in_the_project_config_warns_naming_the_person_files() {
        let d = project("pack: p\nactor: human:all\n", None);
        let e = effective(d.path(), None).expect("warning, not E_CONFIG");
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        assert_eq!(
            e.warnings[0].message,
            "actor in .rdy.yaml belongs in .rdy/local.yaml or the user config; rdy ignores it here"
        );
        assert_eq!(
            e.warnings[0].fix,
            "move actor to .rdy/local.yaml or the user config"
        );
    }

    #[test]
    fn hd9_the_home_names_the_user_file_when_it_is_known() {
        let d = project("pack: p\nactor: human:all\n", None);
        let u = tempfile::tempdir().unwrap();
        let uf = u.path().join("config.yaml");
        let w = effective(d.path(), Some(&uf)).unwrap().warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        let home = format!(".rdy/local.yaml or the user config ({})", uf.display());
        assert_eq!(
            w[0].message,
            format!("actor in .rdy.yaml belongs in {home}; rdy ignores it here")
        );
        assert_eq!(w[0].fix, format!("move actor to {home}"));
    }

    #[test]
    fn hd9_the_wrong_file_wins_over_a_wrong_type_and_a_bad_section() {
        for (local, named) in [
            (
                "reviews:\n  require_up_to_date: maybe\n",
                "reviews.require_up_to_date",
            ),
            ("reviews: 5\n", "reviews"),
        ] {
            let d = project("pack: p\n", Some(local));
            let w = effective(d.path(), None)
                .unwrap_or_else(|e| panic!("{local:?}: {e}"))
                .warnings;
            assert_eq!(w.len(), 1, "{local:?}: {w:?}");
            assert_eq!(w[0].code, "W_CONFIG_UNKNOWN");
            assert_eq!(
                w[0].message,
                format!(
                    "{named} in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here, so the review gate would stay off"
                ),
                "{local:?}"
            );
            assert_eq!(w[0].fix, format!("move {named} to .rdy.yaml"));
        }
        let d = project("pack: p\n", Some("reviews: {}\n"));
        assert!(
            effective(d.path(), None).unwrap().warnings.is_empty(),
            "an empty section stays silent"
        );
    }

    /// guard: a wrong type on a known key in its own file is still E_CONFIG.
    #[test]
    fn hd9_a_wrong_type_in_the_right_file_stays_e_config() {
        let d = project("pack: p\nreviews:\n  require_up_to_date: maybe\n", None);
        assert_eq!(effective(d.path(), None).unwrap_err().code, E_CONFIG);
        let d = project("pack: p\n", Some("serve:\n  idle_timeout: soon\n"));
        assert_eq!(effective(d.path(), None).unwrap_err().code, E_CONFIG);
    }

    #[test]
    fn cfgl1_a_flow_line_holds_a_list_on_one_line() {
        let v: Value = serde_yaml_ng::from_str("[../a, b]").unwrap();
        assert_eq!(flow(&v), "[../a, b]");
    }

    #[test]
    fn se2_lease_ttl_is_a_project_key_in_minutes_with_default_10() {
        let d = project("pack: p\n", None);
        let e = effective(d.path(), None).unwrap();
        let o = e
            .origins
            .iter()
            .find(|o| o.key == "reviews.lease_ttl")
            .unwrap();
        assert_eq!(o.value_line(), "10");
        let d = project("pack: p\nreviews:\n  lease_ttl: 3\n", None);
        assert_eq!(
            crate::config::read_config(d.path())
                .unwrap()
                .lease_ttl_minutes(),
            3
        );
        let d = project("pack: p\nreviews:\n  lease_ttl: 0\n", None);
        assert_eq!(
            crate::config::read_config(d.path())
                .unwrap()
                .lease_ttl_minutes(),
            0
        );
    }

    #[test]
    fn se2_a_lease_ttl_that_is_not_a_whole_number_is_e_config() {
        let d = project("pack: p\nreviews:\n  lease_ttl: soon\n", None);
        assert_eq!(effective(d.path(), None).unwrap_err().code, E_CONFIG);
    }

    #[test]
    fn se2_a_near_miss_of_lease_ttl_warns_about_leases_not_the_gate() {
        let d = project("pack: p\nreviews:\n  lease_tl: 3\n", None);
        let w = &effective(d.path(), None).unwrap().warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            w.message,
            "unknown review setting reviews.lease_tl in .rdy.yaml; leases keep the default of 10 minutes"
        );
        // guard: a near miss of the gate key keeps today's text
        let d = project("pack: p\nreviews:\n  require_uptodate: true\n", None);
        assert!(
            effective(d.path(), None).unwrap().warnings[0]
                .message
                .ends_with("the review gate would stay off")
        );
    }

    #[test]
    fn se2_lease_ttl_in_the_local_file_warns_and_names_its_home() {
        let d = project("pack: p\n", Some("reviews:\n  lease_ttl: 3\n"));
        let w = &effective(d.path(), None).unwrap().warnings[0];
        assert_eq!(
            w.message,
            "reviews.lease_ttl in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here, so leases keep the default of 10 minutes"
        );
    }

    #[test]
    fn ls4_a_site_port_out_of_range_warns_and_is_dropped() {
        let d = project("pack: p\nserve:\n  site_port: 80\n", None);
        let e = effective_lenient(d.path(), None);
        assert_eq!(e.warnings.len(), 1);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            e.warnings[0].message,
            "serve.site_port must be 1024–65535; using a port derived from the project path"
        );
        assert_eq!(
            e.warnings[0].fix,
            "set serve.site_port to a whole number from 1024 to 65535 in .rdy/local.yaml"
        );
        assert!(get_path(&e.merged, "serve.site_port").is_none());
    }

    #[test]
    fn ls4_wrong_types_are_config_errors() {
        let d = project("pack: p\nserve:\n  site_port: eighty\n", None);
        let e = effective(d.path(), None).unwrap_err();
        assert_eq!(e.code, "E_CONFIG");
        assert!(
            e.message.starts_with("serve.site_port in .rdy.yaml: "),
            "{}",
            e.message
        );
        let d = project("pack: p\nserve:\n  site_lan: sometimes\n", None);
        let e = effective(d.path(), None).unwrap_err();
        assert!(
            e.message.starts_with("serve.site_lan in .rdy.yaml: "),
            "{}",
            e.message
        );
        let d = project("pack: p\n", Some("serve:\n  site_lan_address: 5\n"));
        let e = effective(d.path(), None).unwrap_err();
        assert!(
            e.message
                .starts_with("serve.site_lan_address in .rdy/local.yaml: "),
            "{}",
            e.message
        );
    }

    #[test]
    fn ls4_a_misspelt_lan_key_warns() {
        let d = project("pack: p\nserve:\n  site_lna: true\n", None);
        let e = effective_lenient(d.path(), None);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert!(
            e.warnings[0].fix.contains("serve.site_lan"),
            "{}",
            e.warnings[0].fix
        );
    }

    #[test]
    fn ls4_a_lan_address_must_be_private_and_needs_lan_mode() {
        let d = project(
            "pack: p\n",
            Some("serve:\n  site_lan: true\n  site_lan_address: 8.8.8.8\n"),
        );
        let e = effective_lenient(d.path(), None);
        assert_eq!(e.warnings.len(), 1);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            e.warnings[0].message,
            "serve.site_lan_address must be a private IPv4 address of this machine; using the default route's address"
        );
        assert_eq!(
            e.warnings[0].fix,
            "set serve.site_lan_address to a private IPv4 address of this machine in .rdy/local.yaml, or remove it"
        );
        assert!(get_path(&e.merged, "serve.site_lan_address").is_none());
        let d = project(
            "pack: p\n",
            Some("serve:\n  site_lan: true\n  site_lan_address: 192.168.1.20\n"),
        );
        assert!(effective_lenient(d.path(), None).warnings.is_empty());
        let d = project(
            "pack: p\n",
            Some("serve:\n  site_lan_address: 192.168.1.20\n"),
        );
        let e = effective_lenient(d.path(), None);
        assert_eq!(e.warnings.len(), 1);
        assert_eq!(
            e.warnings[0].message,
            "serve.site_lan_address has no effect while serve.site_lan is false"
        );
        assert_eq!(
            e.warnings[0].fix,
            "set serve.site_lan to true in .rdy/local.yaml, or remove serve.site_lan_address"
        );
        let d = project(
            "pack: p\nserve:\n  site_lan: true\n  site_lan_address: 192.168.1.20\n",
            None,
        );
        let e = effective_lenient(d.path(), None);
        assert!(
            e.warnings
                .iter()
                .any(|w| w.code == "W_CONFIG_UNKNOWN"
                    && w.message.contains("serve.site_lan_address")),
            "the shared file is the wrong level"
        );
    }

    #[test]
    fn di11_propose_keys_take_a_choice_and_a_count_at_every_level() {
        let d = project(
            "pack: p\npropose:\n  cadence: task\n  changes: 5\n",
            Some("propose:\n  cadence: asked\n"),
        );
        let e = effective(d.path(), None).unwrap();
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        let row = |k: &str| e.origins.iter().find(|o| o.key == k).unwrap().clone();
        assert_eq!(row("propose.cadence").value, Value::from("asked"));
        assert_eq!(row("propose.cadence").level, Level::Local);
        assert_eq!(row("propose.changes").value, Value::from(5));
        assert_eq!(row("propose.changes").level, Level::Project);
        // the defaults
        let d = project("pack: p\n", None);
        let e = effective(d.path(), None).unwrap();
        let row = |k: &str| e.origins.iter().find(|o| o.key == k).unwrap().clone();
        assert_eq!(row("propose.cadence").value, Value::from("break"));
        assert_eq!(row("propose.cadence").level, Level::Default);
        assert_eq!(row("propose.changes").value, Value::from(10));
    }

    #[test]
    fn di11_a_wrong_propose_value_warns_and_a_wrong_type_is_an_error() {
        let d = project(
            "pack: p\npropose:\n  cadence: sometimes\n  changes: 0\n",
            None,
        );
        let e = effective(d.path(), None).unwrap();
        let messages: Vec<&str> = e.warnings.iter().map(|w| w.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "propose.cadence must be break, task, changes or asked; using break",
                "propose.changes must be 1–1000; using 10"
            ]
        );
        assert!(e.warnings.iter().all(|w| w.code == "W_CONFIG_UNKNOWN"));
        assert_eq!(
            e.warnings[0].fix,
            "set propose.cadence to break, task, changes or asked"
        );
        assert_eq!(
            e.warnings[1].fix,
            "set propose.changes to a whole number from 1 to 1000"
        );
        // the default applies at the level that holds the wrong value
        assert_eq!(
            get_path(&e.merged, "propose.cadence"),
            Some(&Value::from("break"))
        );
        assert_eq!(
            get_path(&e.merged, "propose.changes"),
            Some(&Value::from(10))
        );
        let row = |k: &str| e.origins.iter().find(|o| o.key == k).unwrap().clone();
        assert_eq!(row("propose.cadence").level, Level::Project);
        assert_eq!(row("propose.changes").level, Level::Project);
        // and it wins over a lower level: a person's wrong word is `break`, not the project's
        let d = project(
            "pack: p\npropose:\n  cadence: task\n  changes: 4\n",
            Some("propose:\n  cadence: Always\n  changes: 1001\n"),
        );
        let e = effective(d.path(), None).unwrap();
        assert_eq!(e.warnings.len(), 2, "{:?}", e.warnings);
        let row = |k: &str| e.origins.iter().find(|o| o.key == k).unwrap().clone();
        assert_eq!(row("propose.cadence").value, Value::from("break"));
        assert_eq!(row("propose.cadence").level, Level::Local);
        assert_eq!(row("propose.changes").value, Value::from(10));
        assert_eq!(row("propose.changes").level, Level::Local);
        // a negative whole number is a wrong value, not a wrong type
        let d = project("pack: p\npropose:\n  changes: -4\n", None);
        let e = effective(d.path(), None).unwrap();
        assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
        // a wrong YAML type is E_CONFIG
        for bad in [
            "cadence: [a]",
            "cadence: 7",
            "changes: many",
            "changes: 2.5",
        ] {
            let d = project(&format!("pack: p\npropose:\n  {bad}\n"), None);
            let err = effective(d.path(), None).unwrap_err();
            assert_eq!(err.code, E_CONFIG, "{bad}: {}", err.message);
        }
    }

    /// Hub sign-in runs with no project: the user config alone gets every check a project's
    /// warnings run on it (an ordinary unknown key and a hub key alike), the same warnings as
    /// next to a project that adds none, and a file that is missing or does not parse adds none.
    #[test]
    fn hsa8_user_config_warnings_cover_the_user_file_alone() {
        let u = tempfile::tempdir().unwrap();
        let uf = u.path().join("config.yaml");
        assert!(user_config_warnings(&uf).is_empty(), "a missing file");
        std::fs::write(
            &uf,
            "actor: human:qa\nactro: human:wrong\nhub:\n  instalss: {}\n",
        )
        .unwrap();
        let alone = user_config_warnings(&uf);
        let paths: Vec<bool> = ["actro", "hub.instalss"]
            .iter()
            .map(|k| alone.iter().any(|w| w.message.contains(k)))
            .collect();
        assert_eq!(paths, [true, true], "{alone:?}");
        assert!(
            alone.iter().all(|w| w.code == "W_CONFIG_UNKNOWN"),
            "{alone:?}"
        );
        let quiet = project("pack: p\n", None);
        assert_eq!(alone, config_warnings(quiet.path(), Some(&uf)));
        std::fs::write(&uf, "actro: [unclosed\n").unwrap();
        assert!(
            user_config_warnings(&uf).is_empty(),
            "a file that does not parse"
        );
    }
}
