//! Settings come from four levels (FW-X5, D-C5955), minus the file move: `.rdy/local.yaml`,
//! the project config (`.rdy/config.yaml` until M2f-b), the user config and the defaults. The
//! most specific level that sets a key wins. Each key of [`KEY_LEVELS`] names the levels that may
//! set it (R-P6); a key in a file of another level is `E_CONFIG` naming the right file. Keys
//! outside the table are kept as they are and never shown; each one is a `W_CONFIG_UNKNOWN`
//! warning naming the key, the file and the level (R-P6). A mapping key that holds a `.` is never
//! known: rdy reads only nested keys. An unknown key meant as a review setting gets its own
//! warning text (#832): one under `reviews` or nearest to a `reviews` key, a dotted `reviews.` key,
//! and one anywhere whose last segment is (or is within 2 edits of) a `reviews` leaf. The text says
//! that the review gate would stay off. Founder ruling (config typos warn): it is a warning, never
//! `E_CONFIG`, and the project opens.

use crate::{E_CONFIG, E_IO, E_NO_PROJECT, RdyError};
use rdy_schema::{Issue, Severity};
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

/// The project config, read by `read_config`, `parse_config`, `serve_config` and
/// `pack_fingerprint`. M2f-b changes this constant.
pub const PROJECT_FILE: &str = ".rdy/config.yaml";

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
    /// Not read by `Config` yet: any value.
    Any,
}

impl Ty {
    /// `Ok` when `v` has this type, else the message of the deserializer.
    fn check(self, v: &Value) -> Result<(), String> {
        let r = match self {
            Ty::Any => return Ok(()),
            Ty::Str => serde_yaml_ng::from_value::<String>(v.clone()).map(drop),
            Ty::OptStr => serde_yaml_ng::from_value::<Option<String>>(v.clone()).map(drop),
            Ty::Bool => serde_yaml_ng::from_value::<bool>(v.clone()).map(drop),
            Ty::U32 => serde_yaml_ng::from_value::<u32>(v.clone()).map(drop),
            Ty::OptU64 => serde_yaml_ng::from_value::<Option<u64>>(v.clone()).map(drop),
        };
        r.map_err(|e| e.to_string())
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
        key: "reviews.require_up_to_date",
        levels: PROJECT_ONLY,
        ty: Ty::Bool,
        default: Some("false"),
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
    // the default is `human:$USER`, taken from the environment in `effective`
    KeyRule {
        key: "actor",
        levels: PERSON,
        ty: Ty::OptStr,
        default: None,
    },
];

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
    pub origins: Vec<Origin>,
    /// What `Config` is read from: the project file's keys as they are, each key of the table
    /// replaced by its winning value (a default is not written in; the accessors have it).
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
fn file_name(level: Level, user_file: Option<&Path>) -> String {
    match level {
        Level::Local => LOCAL_FILE.to_string(),
        Level::Project => PROJECT_FILE.to_string(),
        Level::User | Level::Default => user_file.map_or_else(
            || "$XDG_CONFIG_HOME/rdy/config.yaml".to_string(),
            |f| f.display().to_string(),
        ),
    }
}

/// How the fix of a wrong-level key names a file.
fn file_fix_name(level: Level, user_file: Option<&Path>) -> String {
    match level {
        Level::Local => format!("\"{LOCAL_FILE}\""),
        Level::Project => format!("\"the project config ({PROJECT_FILE})\""),
        Level::User | Level::Default => {
            format!("\"the user config ({})\"", file_name(level, user_file))
        }
    }
}

fn path_of(project: &Path, level: Level, user_file: Option<&Path>) -> Option<PathBuf> {
    match level {
        Level::Local => Some(project.join(LOCAL_FILE)),
        Level::Project => Some(project.join(PROJECT_FILE)),
        Level::User => user_file.map(Path::to_path_buf),
        Level::Default => None,
    }
}

/// A file read as a mapping of settings: `None` if it does not exist; an empty file is an empty
/// mapping.
fn read_file(path: &Path, shown: &str) -> Result<Option<Mapping>, RdyError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(RdyError::new(
                E_IO,
                format!("{shown}: {e}"),
                "check the file's permissions",
            ));
        }
    };
    parse_file(&text, shown).map(Some)
}

/// A file's bytes, already read, as a mapping of settings (`read_file` without the read).
fn parse_bytes(bytes: &[u8], shown: &str) -> Result<Mapping, RdyError> {
    let text = std::str::from_utf8(bytes).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("{shown}: stream did not contain valid UTF-8 ({e})"),
            "check the file's permissions",
        )
    })?;
    parse_file(text, shown)
}

/// A file's text as a mapping of settings; an empty file is an empty mapping.
fn parse_file(text: &str, shown: &str) -> Result<Mapping, RdyError> {
    let fix = format!("fix {shown}");
    match serde_yaml_ng::from_str::<Value>(text) {
        Ok(Value::Null) => Ok(Mapping::new()),
        Ok(Value::Mapping(m)) => Ok(m),
        Ok(_) => Err(RdyError::new(
            E_CONFIG,
            format!("{shown} must be a mapping of settings"),
            fix,
        )),
        Err(e) => Err(RdyError::new(E_CONFIG, e.to_string(), fix)),
    }
}

/// The settings of the project at `project` with the user config at `user_file`; refuses a key in
/// a file of a level that does not allow it, a file that is not a mapping and a project that has
/// no config.
pub fn effective(project: &Path, user_file: Option<&Path>) -> Result<Effective, RdyError> {
    build(project, user_file, false, None)
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
    build(project, user_file, false, Some(given))
}

/// `effective` that never fails: a key at the wrong level, or a file that does not read, is
/// left out, so a client can still start a daemon that then reports the error.
pub fn effective_lenient(project: &Path, user_file: Option<&Path>) -> Effective {
    build(project, user_file, true, None).unwrap_or_else(|_| Effective {
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

/// `config_warnings` over the environment's user config.
pub fn config_warnings_for_env(project: &Path) -> Vec<Issue> {
    config_warnings(project, user_file_from_env().as_deref())
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

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The key of the table `level` allows that is nearest to `path`, if it is within 2 edits.
fn nearest_key(path: &str, level: Level) -> Option<&'static str> {
    KEY_LEVELS
        .iter()
        .filter(|r| r.levels.contains(&level))
        .map(|r| (edit_distance(path, r.key), r.key))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

/// The fix text of an unknown key: the nearest known key, else remove it.
fn unknown_fix(path: &str, level: Level, file: &str) -> String {
    match nearest_key(path, level) {
        Some(k) => format!("did you mean {k}? rename {path} in {file}"),
        None => format!("remove {path} from {file}, or check its spelling"),
    }
}

/// The `W_CONFIG_UNKNOWN` warnings of `map`, one per unknown path. A key meant as a review
/// setting gets the review texts (#832): the review gate would stay off. That is a dotted key under
/// `reviews`, an unknown key under `reviews` or nearest to a `reviews` key, and a key anywhere
/// whose last segment is (or is within 2 edits of) the leaf of a `reviews` key. Founder ruling
/// (config typos warn): a warning, never `E_CONFIG`, so the project opens.
fn unknown_warnings(level: Level, map: &Mapping, user_file: Option<&Path>) -> Vec<Issue> {
    let mut paths = Vec::new();
    unknown_paths(map, "", &mut paths);
    let file = file_name(level, user_file);
    paths
        .into_iter()
        .map(|u| {
            let (message, fix) = unknown_text(level, &file, &u);
            Issue {
                severity: Severity::Warning,
                code: "W_CONFIG_UNKNOWN",
                block: None,
                message,
                fix,
            }
        })
        .collect()
}

/// The leaf of the `reviews` key whose leaf `segment` is, or is within 2 edits of.
fn review_leaf(segment: &str) -> Option<&'static str> {
    KEY_LEVELS
        .iter()
        .filter_map(|r| r.key.strip_prefix("reviews."))
        .map(|leaf| (edit_distance(segment, leaf), leaf))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, leaf)| leaf)
}

/// The message and fix of unknown path `u` in `file` (`level`): the review texts for a key meant
/// as a review setting, else the plain ones.
fn unknown_text(level: Level, file: &str, u: &Unknown) -> (String, String) {
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
    let unknown = || {
        (
            format!("unknown review setting {path} in {file}; the review gate would stay off"),
            unknown_fix(path, level, file),
        )
    };
    if path.starts_with("reviews.") {
        return unknown();
    }
    let last = path.rsplit('.').next().unwrap_or(path);
    if let Some(leaf) = review_leaf(last) {
        return (
            format!(
                "unknown review setting {path} in {file}; the review gate would stay off; did you \
                 mean reviews.{leaf}?"
            ),
            format!("move it under `reviews:`{home}"),
        );
    }
    if nearest_key(path, level).is_some_and(|k| k.starts_with("reviews.")) {
        return unknown();
    }
    (
        format!(
            "unknown key {path} in {file} ({} level); rdy ignores it",
            level_name(level)
        ),
        match &u.dotted {
            Some(v) => nested_fix(path, v, ""),
            None => unknown_fix(path, level, file),
        },
    )
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

fn build(
    project: &Path,
    user_file: Option<&Path>,
    lenient: bool,
    given: Option<GivenFiles<'_>>,
) -> Result<Effective, RdyError> {
    if !lenient && given.is_none() && !project.join(PROJECT_FILE).exists() {
        return Err(RdyError::new(
            E_NO_PROJECT,
            format!("{} is not an rdy project", project.display()),
            "run rdy init in the project directory",
        ));
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
    let warnings: Vec<Issue> = files
        .iter()
        .flat_map(|(level, map)| unknown_warnings(*level, map, user_file))
        .collect();
    for (level, map) in &mut files {
        for rule in KEY_LEVELS {
            if rule.levels.contains(level) || get_path(map, rule.key).is_none() {
                continue;
            }
            if !lenient {
                let allowed: Vec<String> = rule
                    .levels
                    .iter()
                    .map(|l| file_fix_name(*l, user_file))
                    .collect();
                return Err(RdyError::new(
                    E_CONFIG,
                    format!(
                        "{} is not allowed in {}",
                        rule.key,
                        file_name(*level, user_file)
                    ),
                    format!("set {} in {}", rule.key, allowed.join(" or ")),
                ));
            }
            remove_path(map, rule.key);
        }
    }
    for (level, map) in &mut files {
        for rule in KEY_LEVELS {
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
    let mut merged = files
        .iter()
        .find(|(l, _)| *l == Level::Project)
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    let mut origins = Vec::new();
    for rule in KEY_LEVELS {
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
            "actor" => std::env::var("USER")
                .ok()
                .filter(|u| !u.is_empty())
                .map(|u| Value::from(format!("human:{u}"))),
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
    Ok(Effective {
        origins,
        merged,
        warnings,
    })
}

/// The `actor` a file names: a non-empty string, else nothing (a file that does not read or
/// parse names none).
fn actor_in(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_yaml_ng::from_str(&text).ok()?;
    v.get("actor")
        .and_then(Value::as_str)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// The human actor of the project: `actor` in `.rdy/local.yaml`, else in the user config, else
/// `human:<user>`. Never an error.
pub fn human_actor_for(project: &Path, user_file: Option<&Path>, user: Option<&str>) -> String {
    actor_in(&project.join(LOCAL_FILE))
        .or_else(|| user_file.and_then(actor_in))
        .unwrap_or_else(|| format!("human:{}", user.unwrap_or("unknown")))
}

/// `human_actor_for` over `XDG_CONFIG_HOME`, `HOME` and `USER`: the one place product code reads
/// them.
pub fn human_actor_for_env(project: &Path) -> String {
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
                .starts_with("reviews.require_up_to_date in .rdy/config.yaml: "),
            "{e}"
        );
        let d = project("pack: p\nserve: 5\n", None);
        let e = effective(d.path(), None).unwrap_err();
        assert!(
            e.message
                .starts_with("serve.start_timeout in .rdy/config.yaml: "),
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
                && w.message.contains(".rdy/config.yaml")
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
                "unknown review setting reviews.require_uptodate in .rdy/config.yaml; the review gate would stay off"
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
        assert!(
            w[0].message
                .contains("did you mean reviews.require_up_to_date?"),
            "{}",
            w[0].message
        );
        assert_eq!(w[0].fix, "move it under `reviews:` in .rdy/config.yaml");
        let d = project("pack: p\n", Some("reviews.require_up_to_date: true\n"));
        let w = effective(d.path(), None).unwrap().warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            w[0].fix,
            "write it nested in .rdy/config.yaml: `reviews:` then `  require_up_to_date: true` on the next line"
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
    fn cfgw1_a_known_key_at_the_wrong_level_is_still_e_config() {
        let d = project("pack: p\n", Some("pack: x\n"));
        let e = effective(d.path(), None).unwrap_err();
        assert_eq!(e.code, E_CONFIG);
        let d = project("pack: p\n", Some("decisions:\n  home: x\n"));
        assert_eq!(effective(d.path(), None).unwrap_err().code, E_CONFIG);
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
    fn cfgl1_a_flow_line_holds_a_list_on_one_line() {
        let v: Value = serde_yaml_ng::from_str("[../a, b]").unwrap();
        assert_eq!(flow(&v), "[../a, b]");
    }
}
