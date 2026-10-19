//! Remote and install settings (D-B8N21, D-C5955) and the separate credential codec (n-XZDQD).
//!
//! `remotes.<name>` is `{url, mode}` and may come from any level; a whole named entry at a more
//! specific level replaces a less specific one (user, then `.rdy.yaml`, then `.rdy/local.yaml`).
//! `hub.installs.<origin>` holds `ca_file` (user config only) and `pinned_origins` (user config or
//! `.rdy/local.yaml`); its fields merge user then local, and a local list replaces the user list.
//! No hub setting comes from the shared `.rdy.yaml`. An unknown, wrong-level or invalid field is a
//! `W_CONFIG_UNKNOWN` warning and is dropped before the merge: it never reaches a value, a display
//! row or a diagnostic. A warning prints only static key names and dynamic keys that passed their
//! check; a reserved secret spelling is always `hub.installs.credential`.
//!
//! Tokens and device codes live only in the user config's sibling `credentials.yaml`, which only
//! [`parse_credentials`] and [`encode_credentials`] read and write. Every helper here is pure: it
//! never reads a path or logs its input, and the caller reads the bytes under its file lock. Both
//! codecs bound their input (1 MiB, depth 32, 4096 entries a document, 16384-byte scalars, no
//! alias, no tagged key) before the semantic parse, and rely on serde_yaml_ng's own duplicate-key
//! refusal. Encoding checks the same bounds on the borrowed value and writes through a buffer
//! that stops at 1 MiB.
use crate::config_levels::{
    self, LOCAL_FILE, Level, Origin as SettingOrigin, PROJECT_FILE, is_repo_name,
};
use crate::did_you_mean::nearest;
use crate::error::{
    E_BAD_INPUT, E_CONFIG, E_LIMIT, E_NOT_FOUND, RdyError, remote_exists, remote_required,
};
use crate::hub_types::{
    INITIAL_POLL_SECONDS, LimitUnit, MAX_ACTOR_BYTES, MAX_CONFIG_BYTES, MAX_DEPTH,
    MAX_DEVICE_CODE_BYTES, MAX_ID_BYTES, MAX_INSTALLS, MAX_MEMBERS, MAX_ORIGINS, MAX_POLL_SECONDS,
    MAX_REMOTES, MAX_SCALAR_BYTES, Origin, OriginFence, RemoteUrl, Secret, check_machine_name,
    check_user_code, is_device_page, token_form,
};
use rdy_schema::{Issue, Severity};
use saphyr_parser::{Event, Parser, ScalarStyle};
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The remotes section.
pub const REMOTES_KEY: &str = "remotes";
/// The hub section.
pub const HUB_KEY: &str = "hub";
/// The installs under the hub section, by canonical origin.
pub const HUB_INSTALLS_KEY: &str = "hub.installs";
/// The credential file's name, beside the user config.
pub const CREDENTIAL_FILE: &str = "credentials.yaml";

/// The message of a settings file or input that does not parse within its bounds.
pub const SETTINGS_MESSAGE: &str = "config is not a valid bounded mapping";
/// The fix of [`SETTINGS_MESSAGE`].
pub const SETTINGS_FIX: &str = "fix the config syntax, duplicate keys or aliases";
/// The message of a credential file that does not parse, or holds a wrong field or state.
pub const CREDENTIALS_MESSAGE: &str = "the credential file is not a valid bounded mapping";
/// The fix of [`CREDENTIALS_MESSAGE`].
pub const CREDENTIALS_FIX: &str = "remove the invalid credential file, then sign in again";
/// The message of an unknown credential field. It never names the field.
pub const UNKNOWN_CREDENTIAL_MESSAGE: &str = "unknown credential field";
/// The fix of a config input over a limit.
pub const CONFIG_LIMIT_FIX: &str = "reduce the config file to the stated limit";
/// The message of a remote name that fails [`is_remote_name`].
pub const BAD_REMOTE_NAME_MESSAGE: &str = "the remote name is not valid";
/// The fix of [`BAD_REMOTE_NAME_MESSAGE`].
pub const BAD_REMOTE_NAME_FIX: &str =
    "use 1 to 64 letters, digits, `.`, `_` or `-`, starting with a letter or digit";

/// The one diagnostic path of every reserved secret spelling.
const SECRET_PATH: &str = "hub.installs.credential";
/// The reserved secret spellings: never a setting, never printed as a leaf.
const SECRET_WORDS: [&str; 4] = ["credential", "access_token", "refresh_token", "device_code"];

const URL_FORM: &str = "an HTTPS URL";
const MODE_FORM: &str = "share or publish";
const CA_FORM: &str = "an absolute CA file path";
const ORIGINS_FORM: &str = "a list of unique HTTPS origins";
const NAME_FORM: &str = "a remote name";
const MAP_FORM: &str = "a map within the stated limits";
const USER_HOME: &str = "user config";
const PERSON_HOME: &str = "user config or .rdy/local.yaml";

const CA_FILE: &str = "ca_file";
const PINNED_ORIGINS: &str = "pinned_origins";
const INSTALL_FIELDS: [&str; 2] = [CA_FILE, PINNED_ORIGINS];
const REMOTE_FIELDS: [&str; 2] = ["url", "mode"];

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// `E_LIMIT` for a config or credential input over `limit` `unit`.
pub fn config_limit(limit: usize, unit: LimitUnit) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("config input exceeds {limit} {}", unit.as_str()),
        CONFIG_LIMIT_FIX,
    )
}

/// `E_CONFIG` for settings that do not parse within their bounds: `file` names the file, and
/// nothing from it is quoted.
pub(crate) fn settings_invalid(file: Option<&str>) -> RdyError {
    let message = match file {
        Some(f) => format!("{SETTINGS_MESSAGE} in {f}"),
        None => SETTINGS_MESSAGE.to_string(),
    };
    RdyError::new(E_CONFIG, message, SETTINGS_FIX)
}

fn credentials_invalid() -> RdyError {
    RdyError::new(E_CONFIG, CREDENTIALS_MESSAGE, CREDENTIALS_FIX)
}

/// An unknown credential field, named only by the known field nearest to it.
fn unknown_credential(known: Option<&str>) -> RdyError {
    let fix = match known {
        Some(k) => format!("did you mean {k}? use that field"),
        None => "remove the unknown field".to_string(),
    };
    RdyError::new(E_CONFIG, UNKNOWN_CREDENTIAL_MESSAGE, fix)
}

fn bad_remote_name() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_REMOTE_NAME_MESSAGE, BAD_REMOTE_NAME_FIX)
}

/// `E_BAD_INPUT` for a typed edit whose value is not `form`.
fn edit_invalid(path: &str, form: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("invalid value for {path}"),
        format!("set {path} to {form}"),
    )
}

// ---------------------------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------------------------

/// Why a YAML text was refused before its semantic parse.
pub(crate) enum Refusal {
    /// Syntax, an alias or a duplicate key: the caller's own `E_CONFIG` text.
    Syntax,
    /// Over a resource bound: [`config_limit`].
    Limit(usize, LimitUnit),
}

impl Refusal {
    pub(crate) fn into_error(self, syntax: impl FnOnce() -> RdyError) -> RdyError {
        match self {
            Refusal::Syntax => syntax(),
            Refusal::Limit(n, unit) => config_limit(n, unit),
        }
    }
}

/// The resource checks the event scan makes before serde_yaml_ng builds a value: the text size,
/// no alias, no tagged mapping key, at most 32 nested collections, at most 4096 mapping entries
/// and sequence elements in the whole document, and at most 16384 bytes in one scalar. A syntax
/// error the scan meets is [`Refusal::Syntax`]. `Ok(true)` when the text holds a node other than
/// an empty plain scalar: an explicit `null` document, not an empty or comment-only one.
fn scan(text: &str) -> Result<bool, Refusal> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(Refusal::Limit(MAX_CONFIG_BYTES, LimitUnit::Bytes));
    }
    // per open collection: whether it is a mapping, and the child nodes seen
    let mut open: Vec<(bool, usize)> = Vec::new();
    // the mapping entries and sequence elements of the whole document so far
    let mut entries = 0usize;
    let mut explicit = false;
    for ev in Parser::new_from_str(text) {
        let (ev, _) = ev.map_err(|_| Refusal::Syntax)?;
        // counts one node in its collection, and a new entry (a key or an element) in the
        // document; whether it is a mapping key
        let mut child = |open: &mut Vec<(bool, usize)>| -> Result<bool, Refusal> {
            let Some((map, n)) = open.last_mut() else {
                return Ok(false);
            };
            *n += 1;
            let key = *map && *n % 2 == 1;
            if key || !*map {
                entries += 1;
                if entries > MAX_MEMBERS {
                    return Err(Refusal::Limit(MAX_MEMBERS, LimitUnit::Entries));
                }
            }
            Ok(key)
        };
        // a tagged key is not a string key: a key-based removal would miss it
        let tagged_key = |key: bool, tag: bool| -> Result<(), Refusal> {
            if key && tag {
                return Err(Refusal::Syntax);
            }
            Ok(())
        };
        match ev {
            Event::Alias(_) => return Err(Refusal::Syntax),
            Event::Scalar(s, style, _, tag) => {
                tagged_key(child(&mut open)?, tag.is_some())?;
                if s.len() > MAX_SCALAR_BYTES {
                    return Err(Refusal::Limit(MAX_SCALAR_BYTES, LimitUnit::Bytes));
                }
                explicit |= !s.is_empty() || style != ScalarStyle::Plain || tag.is_some();
            }
            Event::SequenceStart(_, ref tag) | Event::MappingStart(_, ref tag) => {
                tagged_key(child(&mut open)?, tag.is_some())?;
                open.push((matches!(ev, Event::MappingStart(..)), 0));
                if open.len() > MAX_DEPTH {
                    return Err(Refusal::Limit(MAX_DEPTH, LimitUnit::Levels));
                }
                explicit = true;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                open.pop();
            }
            _ => {}
        }
    }
    Ok(explicit)
}

/// A YAML text as one value, after [`scan`]. serde_yaml_ng refuses a duplicate mapping key; its
/// message is never kept.
pub(crate) fn parse_bounded(text: &str) -> Result<Value, Refusal> {
    parse_explicit(text).map(|(v, _)| v)
}

/// [`parse_bounded`], and whether the text held a node: `false` for an empty or comment-only
/// document, `true` for an explicit `null`.
fn parse_explicit(text: &str) -> Result<(Value, bool), Refusal> {
    let explicit = scan(text)?;
    let value = serde_yaml_ng::from_str::<Value>(text).map_err(|_| Refusal::Syntax)?;
    Ok((value, explicit))
}

/// The bounds of [`scan`] over a whole document about to be encoded, a borrowed mapping, so it
/// need not be copied into a value first.
fn document_bounds(m: &Mapping) -> Result<(), RdyError> {
    mapping_bounds(m, 0, &mut 0)
}

/// The bounds of [`scan`] over a value about to be encoded; `depth` counts the collections
/// around `v`, and `entries` the document's mapping entries and sequence elements so far.
fn value_bounds(v: &Value, depth: usize, entries: &mut usize) -> Result<(), RdyError> {
    match v {
        Value::Sequence(items) => {
            nested_bounds(items.len(), depth, entries)?;
            items
                .iter()
                .try_for_each(|i| value_bounds(i, depth + 1, entries))
        }
        Value::Mapping(m) => mapping_bounds(m, depth, entries),
        Value::String(s) if s.len() > MAX_SCALAR_BYTES => {
            Err(config_limit(MAX_SCALAR_BYTES, LimitUnit::Bytes))
        }
        Value::Tagged(t) => value_bounds(&t.value, depth, entries),
        _ => Ok(()),
    }
}

/// [`value_bounds`] of a borrowed mapping.
fn mapping_bounds(m: &Mapping, depth: usize, entries: &mut usize) -> Result<(), RdyError> {
    nested_bounds(m.len(), depth, entries)?;
    m.iter().try_for_each(|(k, v)| {
        value_bounds(k, depth + 1, entries)?;
        value_bounds(v, depth + 1, entries)
    })
}

/// A collection of `len` entries inside `depth` others, added to the document's `entries`.
fn nested_bounds(len: usize, depth: usize, entries: &mut usize) -> Result<(), RdyError> {
    if depth + 1 > MAX_DEPTH {
        return Err(config_limit(MAX_DEPTH, LimitUnit::Levels));
    }
    *entries = entries
        .checked_add(len)
        .filter(|n| *n <= MAX_MEMBERS)
        .ok_or_else(|| config_limit(MAX_MEMBERS, LimitUnit::Entries))?;
    Ok(())
}

/// `value` (already within the bounds) as YAML bytes, at most 1 MiB, or `invalid` when it does
/// not encode.
fn encode_bounded<T: Serialize + ?Sized>(
    value: &T,
    invalid: fn() -> RdyError,
) -> Result<Vec<u8>, RdyError> {
    let mut out = BoundedBuf::new(MAX_CONFIG_BYTES);
    write_bounded(value, &mut out, invalid)?;
    Ok(out.buf)
}

/// An output buffer that never holds, or reserves, more than `limit` bytes: a write past it
/// fails and is remembered.
pub(crate) struct BoundedBuf {
    buf: Vec<u8>,
    limit: usize,
    over: bool,
}

impl BoundedBuf {
    pub(crate) fn new(limit: usize) -> BoundedBuf {
        BoundedBuf {
            buf: Vec::new(),
            limit,
            over: false,
        }
    }
}

impl std::io::Write for BoundedBuf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let need = self
            .buf
            .len()
            .checked_add(data.len())
            .filter(|n| *n <= self.limit);
        let Some(need) = need else {
            self.over = true;
            return Err(std::io::Error::other("over the output bound"));
        };
        if need > self.buf.capacity() {
            // grow by doubling, but never past the limit
            let target = need
                .max(self.buf.capacity().saturating_mul(2))
                .min(self.limit);
            self.buf.reserve_exact(target - self.buf.len());
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `value` as YAML into `out`, which stops the encoder at its limit: past it is `E_LIMIT`, any
/// other failure `invalid`. The encoder's own error text is never kept.
pub(crate) fn write_bounded<T: Serialize + ?Sized>(
    value: &T,
    out: &mut BoundedBuf,
    invalid: fn() -> RdyError,
) -> Result<(), RdyError> {
    match serde_yaml_ng::to_writer(&mut *out, value) {
        Ok(()) => Ok(()),
        Err(_) if out.over => Err(config_limit(out.limit, LimitUnit::Bytes)),
        Err(_) => Err(invalid()),
    }
}

fn string_keyed(m: &Mapping) -> bool {
    m.keys().all(Value::is_string)
}

// ---------------------------------------------------------------------------------------------
// Settings bytes and typed edits
// ---------------------------------------------------------------------------------------------

/// A settings file's bytes as a mapping with string keys: empty bytes are an empty mapping.
/// Anything else, or bytes over the bounds, is refused without quoting the input.
pub fn parse_settings(bytes: &[u8]) -> Result<Mapping, RdyError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(config_limit(MAX_CONFIG_BYTES, LimitUnit::Bytes));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| settings_invalid(None))?;
    match parse_bounded(text).map_err(|r| r.into_error(|| settings_invalid(None)))? {
        Value::Null => Ok(Mapping::new()),
        Value::Mapping(m) if string_keyed(&m) => Ok(m),
        _ => Err(settings_invalid(None)),
    }
}

/// A settings mapping as YAML bytes, within the bounds. Comments are not kept. The borrowed
/// mapping is checked, then written through a buffer that stops at 1 MiB; it is never copied.
pub fn encode_settings(map: &Mapping) -> Result<Vec<u8>, RdyError> {
    document_bounds(map)?;
    encode_bounded(map, || settings_invalid(None))
}

/// Whether `s` is a remote name: 1 to 64 ASCII bytes, a letter or digit first, then letters,
/// digits, `.`, `_` and `-`.
pub fn is_remote_name(s: &str) -> bool {
    is_repo_name(s)
}

/// How a remote is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteMode {
    /// The project's shared line: at most one effective share remote.
    Share,
    /// A remote that is published to.
    Publish,
}

impl RemoteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteMode::Share => "share",
            RemoteMode::Publish => "publish",
        }
    }

    pub fn parse(text: &str) -> Option<RemoteMode> {
        match text {
            "share" => Some(RemoteMode::Share),
            "publish" => Some(RemoteMode::Publish),
            _ => None,
        }
    }
}

/// One remote's settings: `{url, mode}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub url: RemoteUrl,
    pub mode: RemoteMode,
}

impl RemoteEntry {
    fn to_value(&self) -> Value {
        let mut m = Mapping::new();
        m.insert(Value::from("url"), Value::from(self.url.as_str()));
        m.insert(Value::from("mode"), Value::from(self.mode.as_str()));
        Value::Mapping(m)
    }
}

/// `E_CONFIG` for a settings section at `path` that the loader does not read as a mapping.
fn container_invalid(path: &str) -> RdyError {
    RdyError::new(
        E_CONFIG,
        format!("invalid value for {path}"),
        format!("set {path} to {MAP_FORM}"),
    )
}

/// The mapping at `key` of `m`, made when absent or null. Any other value, a tagged mapping
/// included, is one the loader drops with a warning: it is `E_CONFIG` for `path`, and `m` is
/// unchanged. (A section is only made when it was absent, so every section below it is absent
/// too and a refusal never follows a change.)
fn child_map<'a>(m: &'a mut Mapping, key: &str, path: &str) -> Result<&'a mut Mapping, RdyError> {
    let slot = m.entry(Value::from(key)).or_insert(Value::Null);
    if matches!(slot, Value::Null) {
        *slot = Value::Mapping(Mapping::new());
    }
    match slot {
        Value::Mapping(inner) => Ok(inner),
        _ => Err(container_invalid(path)),
    }
}

/// Whether `(k, v)` of a remotes section is a share remote as the loader reads it: a remote
/// name, a plain mapping, a valid url and mode `share`.
fn is_share_entry(k: &Value, v: &Value) -> bool {
    let Value::Mapping(e) = v else {
        return false;
    };
    k.as_str().is_some_and(is_remote_name)
        && e.get("url")
            .and_then(Value::as_str)
            .is_some_and(|u| RemoteUrl::parse(u).is_ok())
        && e.get("mode")
            .and_then(Value::as_str)
            .and_then(RemoteMode::parse)
            == Some(RemoteMode::Share)
}

/// Sets remote `name` of one settings mapping to `entry`, or removes it (and a `remotes` section
/// it leaves empty). Every other value stays. A second share remote in the same mapping is
/// `E_REMOTE_EXISTS`, and a `remotes` section that is not a plain mapping `E_CONFIG`; the
/// mapping is then unchanged.
pub fn set_remote(
    map: &mut Mapping,
    name: &str,
    entry: Option<RemoteEntry>,
) -> Result<(), RdyError> {
    if !is_remote_name(name) {
        return Err(bad_remote_name());
    }
    match entry {
        Some(e) => {
            let other_share = e.mode == RemoteMode::Share
                && matches!(map.get(REMOTES_KEY), Some(Value::Mapping(rs)) if rs
                    .iter()
                    .any(|(k, v)| k.as_str() != Some(name) && is_share_entry(k, v)));
            if other_share {
                return Err(remote_exists());
            }
            put_remote(map, name, Some(e))
        }
        None => put_remote(map, name, None),
    }
}

/// [`set_remote`] without its share check, for a caller that checks the single share remote on
/// the merged config instead: one file may hold a second share entry that another level
/// overrides. `name` must be a remote name.
pub(crate) fn put_remote(
    map: &mut Mapping,
    name: &str,
    entry: Option<RemoteEntry>,
) -> Result<(), RdyError> {
    match entry {
        Some(e) => {
            child_map(map, REMOTES_KEY, REMOTES_KEY)?.insert(Value::from(name), e.to_value());
        }
        None => {
            if let Some(Value::Mapping(rs)) = map.get_mut(REMOTES_KEY) {
                rs.shift_remove(name);
                if rs.is_empty() {
                    map.shift_remove(REMOTES_KEY);
                }
            }
        }
    }
    Ok(())
}

/// One field of an install's settings: `Some` sets it, `None` removes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallField {
    /// `ca_file`: an absolute UTF-8 path (user config only).
    CaFile(Option<PathBuf>),
    /// `pinned_origins`: at most 16; written sorted and unique.
    PinnedOrigins(Option<Vec<Origin>>),
}

/// Sets one field of `hub.installs.<install>` in one settings mapping, or removes it (and each
/// section it leaves empty). Every other value stays. A value that is not the field's form is
/// `E_BAD_INPUT`, more than 16 origins `E_LIMIT`, and a section on the way that is not a plain
/// mapping `E_CONFIG`; the mapping is then unchanged.
pub fn set_install_field(
    map: &mut Mapping,
    install: &Origin,
    field: InstallField,
) -> Result<(), RdyError> {
    let path = |key: &str| format!("{HUB_INSTALLS_KEY}.{install}.{key}");
    let (key, value) = match field {
        InstallField::CaFile(Some(p)) => {
            let text = p
                .to_str()
                .filter(|s| p.is_absolute() && s.len() <= MAX_SCALAR_BYTES)
                .ok_or_else(|| edit_invalid(&path(CA_FILE), CA_FORM))?;
            (CA_FILE, Some(Value::from(text)))
        }
        InstallField::CaFile(None) => (CA_FILE, None),
        InstallField::PinnedOrigins(Some(mut list)) => {
            list.sort();
            list.dedup();
            if list.len() > MAX_ORIGINS {
                return Err(config_limit(MAX_ORIGINS, LimitUnit::Origins));
            }
            let seq = list.iter().map(|o| Value::from(o.as_str())).collect();
            (PINNED_ORIGINS, Some(Value::Sequence(seq)))
        }
        InstallField::PinnedOrigins(None) => (PINNED_ORIGINS, None),
    };
    match value {
        Some(v) => {
            let hub = child_map(map, HUB_KEY, HUB_KEY)?;
            let installs = child_map(hub, "installs", HUB_INSTALLS_KEY)?;
            let base = format!("{HUB_INSTALLS_KEY}.{install}");
            child_map(installs, install.as_str(), &base)?.insert(Value::from(key), v);
        }
        None => {
            let Some(Value::Mapping(hub)) = map.get_mut(HUB_KEY) else {
                return Ok(());
            };
            if let Some(Value::Mapping(installs)) = hub.get_mut("installs") {
                if let Some(Value::Mapping(one)) = installs.get_mut(install.as_str()) {
                    one.shift_remove(key);
                    if one.is_empty() {
                        installs.shift_remove(install.as_str());
                    }
                }
                if installs.is_empty() {
                    hub.shift_remove("installs");
                }
            }
            if hub.is_empty() {
                map.shift_remove(HUB_KEY);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Settings at each level
// ---------------------------------------------------------------------------------------------

/// One install's valid fields from one file.
#[derive(Clone, Debug, Default)]
struct InstallPart {
    ca_file: Option<PathBuf>,
    pinned: Option<Vec<Origin>>,
}

/// The valid hub settings of one file.
#[derive(Clone, Debug, Default)]
pub(crate) struct LevelHub {
    remotes: BTreeMap<String, RemoteEntry>,
    installs: BTreeMap<Origin, InstallPart>,
}

/// A hub warning with what it sorts by: the file (user, project, local), then the path.
#[derive(Clone, Debug)]
pub(crate) struct Note {
    rank: u8,
    path: String,
    issue: Issue,
}

fn rank(level: Level) -> u8 {
    match level {
        Level::User => 0,
        Level::Project => 1,
        Level::Local => 2,
        Level::Default => 3,
    }
}

/// The checks of one file's hub subtree and remotes.
struct Walk<'a> {
    level: Level,
    file: &'a str,
    part: LevelHub,
    notes: Vec<Note>,
}

impl Walk<'_> {
    fn note(&mut self, path: &str, message: String, fix: String) {
        self.notes.push(Note {
            rank: rank(self.level),
            path: path.to_string(),
            issue: Issue {
                severity: Severity::Warning,
                code: "W_CONFIG_UNKNOWN",
                block: None,
                message,
                fix,
            },
        });
    }

    /// An unknown setting at `path`; `known` is the nearest known path.
    fn unknown(&mut self, path: &str, known: Option<String>) {
        let fix = match known {
            Some(k) => format!("did you mean {k}? rename {path}"),
            None => format!("remove {path}"),
        };
        self.note(path, format!("unknown hub setting {path}"), fix);
    }

    /// An unknown key `key` under `parent`, whose known keys are `known`: a reserved secret
    /// spelling is [`SECRET_PATH`], whatever its parent, and a dotted key has the path of a
    /// dotted key at the top level ([`dotted_path`]), never its own.
    fn unknown_key(&mut self, parent: &str, key: &str, known: &[&str]) {
        if SECRET_WORDS.contains(&key) {
            self.unknown(SECRET_PATH, None);
            return;
        }
        if key.contains('.') {
            self.unknown(dotted_path(&format!("{parent}.{key}")), None);
            return;
        }
        let near = nearest(key, known).map(|k| format!("{parent}.{k}"));
        self.unknown(&format!("{parent}.{key}"), near);
    }

    fn invalid(&mut self, path: &str, form: &str) {
        self.note(
            path,
            format!("invalid value for {path}"),
            format!("set {path} to {form}"),
        );
    }

    fn not_allowed(&mut self, path: &str, home: &str) {
        let file = self.file;
        self.note(
            path,
            format!("{path} is not allowed in {file}"),
            format!("set {path} in {home} and remove it from {file}"),
        );
    }

    fn hub(&mut self, v: Value) {
        let m = match v {
            Value::Null => return,
            Value::Mapping(m) => m,
            _ => return self.invalid(HUB_KEY, MAP_FORM),
        };
        for (k, v) in m {
            match k.as_str() {
                Some("installs") => self.installs(v),
                Some(key) => self.unknown_key(HUB_KEY, key, &["installs"]),
                None => self.invalid(HUB_KEY, MAP_FORM),
            }
        }
    }

    fn installs(&mut self, v: Value) {
        let m = match v {
            Value::Null => return,
            Value::Mapping(m) => m,
            _ => return self.invalid(HUB_INSTALLS_KEY, MAP_FORM),
        };
        for (k, v) in m {
            match k.as_str() {
                Some(key) if SECRET_WORDS.contains(&key) => self.unknown(SECRET_PATH, None),
                Some(key) => match canonical_origin(key) {
                    Some(origin) => self.install(origin, v),
                    None if has_secret_segment(key) => self.unknown(SECRET_PATH, None),
                    None => self.invalid(HUB_INSTALLS_KEY, MAP_FORM),
                },
                None => self.invalid(HUB_INSTALLS_KEY, MAP_FORM),
            }
        }
    }

    fn install(&mut self, origin: Origin, v: Value) {
        let base = format!("{HUB_INSTALLS_KEY}.{origin}");
        let m = match v {
            Value::Null => return,
            Value::Mapping(m) => m,
            _ => return self.invalid(&base, MAP_FORM),
        };
        let mut part = InstallPart::default();
        for (k, v) in m {
            let Some(key) = k.as_str() else {
                self.invalid(&base, MAP_FORM);
                continue;
            };
            let path = format!("{base}.{key}");
            match key {
                CA_FILE if self.level != Level::User => self.not_allowed(&path, USER_HOME),
                CA_FILE => match v.as_str().filter(|s| Path::new(s).is_absolute()) {
                    Some(s) => part.ca_file = Some(PathBuf::from(s)),
                    None => self.invalid(&path, CA_FORM),
                },
                PINNED_ORIGINS if self.level == Level::Project => {
                    self.not_allowed(&path, PERSON_HOME)
                }
                PINNED_ORIGINS => match origin_list(&v) {
                    Some(list) => part.pinned = Some(list),
                    None => self.invalid(&path, ORIGINS_FORM),
                },
                _ => self.unknown_key(&base, key, &INSTALL_FIELDS),
            }
        }
        if part.ca_file.is_some() || part.pinned.is_some() {
            self.part.installs.insert(origin, part);
        }
    }

    fn remotes(&mut self, v: Value) {
        let m = match v {
            Value::Null => return,
            Value::Mapping(m) => m,
            _ => return self.invalid(REMOTES_KEY, MAP_FORM),
        };
        for (k, v) in m {
            match k.as_str().filter(|n| is_remote_name(n)) {
                Some(name) => self.remote(name, v),
                None => self.invalid(REMOTES_KEY, NAME_FORM),
            }
        }
    }

    fn remote(&mut self, name: &str, v: Value) {
        let base = format!("{REMOTES_KEY}.{name}");
        let Value::Mapping(m) = v else {
            return self.invalid(&base, MAP_FORM);
        };
        let url_path = format!("{base}.url");
        let mode_path = format!("{base}.mode");
        let mut url = None;
        let mut mode = None;
        for (k, v) in &m {
            let Some(key) = k.as_str() else {
                self.invalid(&base, MAP_FORM);
                continue;
            };
            match key {
                "url" => url = v.as_str().and_then(|s| RemoteUrl::parse(s).ok()),
                "mode" => mode = v.as_str().and_then(RemoteMode::parse),
                _ => self.unknown_key(&base, key, &REMOTE_FIELDS),
            }
        }
        // a missing field and an invalid one alike: both are required
        if url.is_none() {
            self.invalid(&url_path, URL_FORM);
        }
        if mode.is_none() {
            self.invalid(&mode_path, MODE_FORM);
        }
        if let (Some(url), Some(mode)) = (url, mode) {
            self.part
                .remotes
                .insert(name.to_string(), RemoteEntry { url, mode });
        }
    }
}

/// `key` as an origin when it is one already in canonical form.
fn canonical_origin(key: &str) -> Option<Origin> {
    Origin::parse(key).ok().filter(|o| o.as_str() == key)
}

/// A list of unique canonical origins, sorted.
fn origin_list(v: &Value) -> Option<Vec<Origin>> {
    let items = v.as_sequence()?;
    let mut list = items
        .iter()
        .map(|i| i.as_str().and_then(canonical_origin))
        .collect::<Option<Vec<Origin>>>()?;
    list.sort();
    let n = list.len();
    list.dedup();
    (list.len() == n).then_some(list)
}

/// Whether a reserved secret spelling is one of `key`'s dot-separated segments.
fn has_secret_segment(key: &str) -> bool {
    key.split('.').any(|seg| SECRET_WORDS.contains(&seg))
}

/// The diagnostic path of a dotted key under the hub or remotes, as its full dotted path: a
/// reserved secret spelling anywhere is [`SECRET_PATH`]; else only the static section, never a
/// dynamic key.
fn dotted_path(key: &str) -> &'static str {
    if has_secret_segment(key) {
        SECRET_PATH
    } else if key.starts_with("remotes.") {
        REMOTES_KEY
    } else if key.starts_with("hub.installs.") || key == HUB_INSTALLS_KEY {
        HUB_INSTALLS_KEY
    } else {
        HUB_KEY
    }
}

/// Takes the hub subtree, the remotes and every top-level dotted key under them out of `map`
/// (the file of `level`, named `file` in messages): the valid settings, and a warning for each
/// field that is unknown, at the wrong level or invalid. rdy reads nothing else of them.
pub(crate) fn take_level(level: Level, map: &mut Mapping, file: &str) -> (LevelHub, Vec<Note>) {
    let mut w = Walk {
        level,
        file,
        part: LevelHub::default(),
        notes: Vec::new(),
    };
    // each entry is removed by its own key, so a key that reads as one of these strings but is
    // not a plain string (the parsers refuse a tagged key) cannot stay behind
    let ours = |k: &str| {
        k == HUB_KEY || k == REMOTES_KEY || k.starts_with("hub.") || k.starts_with("remotes.")
    };
    let keys: Vec<Value> = map
        .keys()
        .filter(|k| k.as_str().is_some_and(ours))
        .cloned()
        .collect();
    let mut hub = Vec::new();
    let mut remotes = Vec::new();
    for k in keys {
        let Some(v) = map.shift_remove(&k) else {
            continue;
        };
        match k.as_str() {
            Some(HUB_KEY) => hub.push(v),
            Some(REMOTES_KEY) => remotes.push(v),
            Some(dotted) => w.unknown(dotted_path(dotted), None),
            None => {}
        }
    }
    hub.into_iter().for_each(|v| w.hub(v));
    remotes.into_iter().for_each(|v| w.remotes(v));
    (w.part, w.notes)
}

/// One remote in effect: its settings, and the level and file that supplied them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: RemoteUrl,
    pub mode: RemoteMode,
    pub level: Level,
    pub file: Option<PathBuf>,
}

/// One install's settings in effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallSettings {
    pub origin: Origin,
    /// From the user config only.
    pub ca_file: Option<PathBuf>,
    /// The local list, else the user list; sorted by canonical bytes.
    pub pinned_origins: Vec<Origin>,
}

/// The hub settings of every level, merged.
pub(crate) struct HubMerge {
    pub(crate) remotes: Vec<Remote>,
    pub(crate) installs: Vec<InstallSettings>,
    /// One display row per remote (`remotes.<name>`), sorted by name.
    pub(crate) remote_rows: Vec<SettingOrigin>,
    /// One display row per install field (`hub.installs.<origin>.<field>`), sorted by origin.
    pub(crate) install_rows: Vec<SettingOrigin>,
    /// The warnings, sorted by file (user, project, local), then by path.
    pub(crate) warnings: Vec<Issue>,
}

impl HubMerge {
    /// The merged remotes as one mapping, when any is set.
    pub(crate) fn remotes_value(&self) -> Option<Value> {
        (!self.remotes.is_empty()).then(|| {
            let m: Mapping = self
                .remotes
                .iter()
                .map(|r| {
                    let e = RemoteEntry {
                        url: r.url.clone(),
                        mode: r.mode,
                    };
                    (Value::from(r.name.as_str()), e.to_value())
                })
                .collect();
            Value::Mapping(m)
        })
    }

    /// The merged installs as one mapping, when any is set.
    pub(crate) fn installs_value(&self) -> Option<Value> {
        (!self.installs.is_empty()).then(|| {
            let m: Mapping = self
                .installs
                .iter()
                .map(|i| {
                    let mut f = Mapping::new();
                    if let Some(ca) = &i.ca_file {
                        f.insert(
                            Value::from(CA_FILE),
                            Value::from(ca.to_string_lossy().into_owned()),
                        );
                    }
                    if !i.pinned_origins.is_empty() {
                        f.insert(
                            Value::from(PINNED_ORIGINS),
                            origins_value(&i.pinned_origins),
                        );
                    }
                    (Value::from(i.origin.as_str()), Value::Mapping(f))
                })
                .collect();
            Value::Mapping(m)
        })
    }
}

fn origins_value(list: &[Origin]) -> Value {
    Value::Sequence(list.iter().map(|o| Value::from(o.as_str())).collect())
}

/// Merges each file's hub settings: remotes by whole entry (user, then project, then local),
/// install fields user then local.
pub(crate) fn merge(
    levels: Vec<(Level, LevelHub)>,
    mut notes: Vec<Note>,
    project: Option<&Path>,
    user_file: Option<&Path>,
) -> HubMerge {
    let file_of = |level: Level| match level {
        Level::User => user_file.map(Path::to_path_buf),
        Level::Project => project.map(|p| p.join(PROJECT_FILE)),
        Level::Local => project.map(|p| p.join(LOCAL_FILE)),
        Level::Default => None,
    };
    let mut levels = levels;
    levels.sort_by_key(|(l, _)| rank(*l));
    let mut remotes: BTreeMap<String, Remote> = BTreeMap::new();
    type Field<T> = Option<(T, Level)>;
    let mut installs: BTreeMap<Origin, (Field<PathBuf>, Field<Vec<Origin>>)> = BTreeMap::new();
    for (level, part) in levels {
        for (name, e) in part.remotes {
            let r = Remote {
                name: name.clone(),
                url: e.url,
                mode: e.mode,
                level,
                file: file_of(level),
            };
            remotes.insert(name, r);
        }
        for (origin, p) in part.installs {
            let slot = installs.entry(origin).or_default();
            if let Some(ca) = p.ca_file {
                slot.0 = Some((ca, level));
            }
            if let Some(list) = p.pinned {
                slot.1 = Some((list, level));
            }
        }
    }
    let remote_rows = remotes
        .values()
        .map(|r| SettingOrigin {
            key: format!("{REMOTES_KEY}.{}", r.name),
            value: RemoteEntry {
                url: r.url.clone(),
                mode: r.mode,
            }
            .to_value(),
            level: r.level,
            file: r.file.clone(),
        })
        .collect();
    let mut install_rows = Vec::new();
    let mut settings = Vec::new();
    for (origin, (ca, pinned)) in installs {
        let row = |field: &str, value: Value, level: Level| SettingOrigin {
            key: format!("{HUB_INSTALLS_KEY}.{origin}.{field}"),
            value,
            level,
            file: file_of(level),
        };
        if let Some((path, level)) = &ca {
            install_rows.push(row(
                CA_FILE,
                Value::from(path.to_string_lossy().into_owned()),
                *level,
            ));
        }
        if let Some((list, level)) = &pinned {
            install_rows.push(row(PINNED_ORIGINS, origins_value(list), *level));
        }
        settings.push(InstallSettings {
            origin: origin.clone(),
            ca_file: ca.map(|(p, _)| p),
            pinned_origins: pinned.map(|(l, _)| l).unwrap_or_default(),
        });
    }
    notes.sort_by(|a, b| (a.rank, &a.path).cmp(&(b.rank, &b.path)));
    HubMerge {
        remotes: remotes.into_values().collect(),
        installs: settings,
        remote_rows,
        install_rows,
        warnings: notes.into_iter().map(|n| n.issue).collect(),
    }
}

/// The hub settings in effect: remotes, installs, their display rows and warnings. It never
/// holds or opens a credential.
#[derive(Clone, Debug)]
pub struct HubConfig {
    remotes: Vec<Remote>,
    installs: Vec<InstallSettings>,
    /// The display rows: remotes by name, then install fields by origin.
    pub origins: Vec<SettingOrigin>,
    /// The hub settings' `W_CONFIG_UNKNOWN` warnings, sorted by file (user, project, local),
    /// then by path.
    pub warnings: Vec<Issue>,
    local_actor: Option<String>,
    user_actor: Option<String>,
}

impl HubConfig {
    /// The hub settings of `project` (`.rdy.yaml` and `.rdy/local.yaml`) and the user config at
    /// `user_file`; with no project, the user config only. Each file is read once, within the
    /// settings bounds, and a missing file is empty. Over 64 remotes, 32 installs or 16 pinned
    /// origins is `E_LIMIT`, a second share remote `E_REMOTE_EXISTS`; the error carries the
    /// warnings as its issues. The credential file is never opened.
    pub fn load(project: Option<&Path>, user_file: Option<&Path>) -> Result<HubConfig, RdyError> {
        let mut files: Vec<(Level, Mapping)> = Vec::new();
        if let Some(uf) = user_file {
            let shown = config_levels::file_name(Level::User, Some(uf));
            if let Some(m) = config_levels::read_file(uf, &shown)? {
                files.push((Level::User, m));
            }
        }
        if let Some(p) = project {
            for (level, rel) in [(Level::Project, PROJECT_FILE), (Level::Local, LOCAL_FILE)] {
                let shown = config_levels::file_name(level, user_file);
                if let Some(m) = config_levels::read_file(&p.join(rel), &shown)? {
                    files.push((level, m));
                }
            }
        }
        let actor_of = |level: Level| {
            files
                .iter()
                .find(|(l, _)| *l == level)
                .and_then(|(_, m)| m.get("actor"))
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
                .map(str::to_string)
        };
        let local_actor = actor_of(Level::Local);
        let user_actor = actor_of(Level::User);
        let mut parts = Vec::new();
        let mut notes = Vec::new();
        for (level, mut map) in files {
            let shown = config_levels::file_name(level, user_file);
            let (part, n) = take_level(level, &mut map, &shown);
            parts.push((level, part));
            notes.extend(n);
        }
        let merged = merge(parts, notes, project, user_file);
        let refuse = |mut e: RdyError| {
            e.issues = merged.warnings.clone();
            Err(e)
        };
        if merged.remotes.len() > MAX_REMOTES {
            return refuse(config_limit(MAX_REMOTES, LimitUnit::Remotes));
        }
        if merged
            .remotes
            .iter()
            .filter(|r| r.mode == RemoteMode::Share)
            .count()
            > 1
        {
            return refuse(remote_exists());
        }
        if merged.installs.len() > MAX_INSTALLS {
            return refuse(config_limit(MAX_INSTALLS, LimitUnit::Installs));
        }
        if merged
            .installs
            .iter()
            .any(|i| i.pinned_origins.len() > MAX_ORIGINS)
        {
            return refuse(config_limit(MAX_ORIGINS, LimitUnit::Origins));
        }
        let mut origins = merged.remote_rows;
        origins.extend(merged.install_rows);
        Ok(HubConfig {
            remotes: merged.remotes,
            installs: merged.installs,
            origins,
            warnings: merged.warnings,
            local_actor,
            user_actor,
        })
    }

    /// The remotes in effect, sorted by name.
    pub fn remotes(&self) -> &[Remote] {
        &self.remotes
    }

    /// The remote named `name`.
    pub fn remote(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|r| r.name == name)
    }

    /// The installs that have settings, sorted by origin.
    pub fn installs(&self) -> &[InstallSettings] {
        &self.installs
    }

    /// The settings of `install`.
    pub fn install(&self, install: &Origin) -> Option<&InstallSettings> {
        self.installs.iter().find(|i| i.origin == *install)
    }

    /// The origin fence of `install`: its own origin and its pinned origins.
    pub fn fence(&self, install: &Origin) -> Result<OriginFence, RdyError> {
        let pinned = self
            .install(install)
            .map(|i| i.pinned_origins.clone())
            .unwrap_or_default();
        OriginFence::new(install.clone(), pinned)
    }

    /// The human actor: `actor` in `.rdy/local.yaml`, else in the user config, else
    /// `human:<user>` (`human:unknown` with no user), as `human_actor_for` reads it, from the
    /// files this config already read within their bounds.
    pub fn human_actor(&self, user: Option<&str>) -> String {
        self.local_actor
            .clone()
            .or_else(|| self.user_actor.clone())
            .unwrap_or_else(|| format!("human:{}", user.unwrap_or("unknown")))
    }

    /// The remote a command uses: `name` when given (`E_NOT_FOUND` when no remote has it), else
    /// the share remote, else the only remote, else `E_REMOTE_REQUIRED`.
    pub fn select_remote(&self, name: Option<&str>) -> Result<Remote, RdyError> {
        if let Some(n) = name {
            if !is_remote_name(n) {
                return Err(bad_remote_name());
            }
            return self.remote(n).cloned().ok_or_else(|| {
                RdyError::new(
                    E_NOT_FOUND,
                    format!("remote {n} does not exist"),
                    "list the remotes, then pass a listed name",
                )
            });
        }
        if let Some(r) = self.remotes.iter().find(|r| r.mode == RemoteMode::Share) {
            return Ok(r.clone());
        }
        match self.remotes.as_slice() {
            [only] => Ok(only.clone()),
            _ => Err(remote_required()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------------------------

/// The credential file of the user config at `user_file`: its sibling `credentials.yaml`. No
/// project or setting changes it.
pub fn credential_file(user_file: &Path) -> PathBuf {
    user_file.with_file_name(CREDENTIAL_FILE)
}

/// A device sign-in in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSignIn {
    pub machine_name: String,
    pub device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at_ms: i64,
    pub next_poll_at_ms: i64,
    pub interval_seconds: u64,
}

/// A signed-in install's tokens and account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyCredential {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_at_ms: i64,
    pub account: String,
    pub handle: String,
}

/// One install's credential. An install with none is signed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    Pending(PendingSignIn),
    Ready(ReadyCredential),
    ReauthRequired,
}

const STATE: &str = "state";
const PENDING_FIELDS: [&str; 8] = [
    STATE,
    "machine_name",
    "device_code",
    "user_code",
    "verification_uri",
    "expires_at_ms",
    "next_poll_at_ms",
    "interval_seconds",
];
const READY_FIELDS: [&str; 6] = [
    STATE,
    "access_token",
    "refresh_token",
    "expires_at_ms",
    "account",
    "handle",
];
const REAUTH_FIELDS: [&str; 1] = [STATE];
const TOP_FIELDS: [&str; 1] = ["installs"];

impl Credential {
    /// The `state` it is stored with.
    pub fn state(&self) -> &'static str {
        match self {
            Credential::Pending(_) => "pending",
            Credential::Ready(_) => "ready",
            Credential::ReauthRequired => "reauth_required",
        }
    }

    /// Whether every field has its form, for `install`.
    fn valid(&self, install: &Origin) -> bool {
        let text = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        match self {
            Credential::Pending(p) => {
                check_machine_name(&p.machine_name).is_ok()
                    && text(p.device_code.expose(), MAX_DEVICE_CODE_BYTES)
                    && check_user_code(&p.user_code).is_ok()
                    && is_device_page(install, &p.verification_uri)
                    && p.expires_at_ms >= 0
                    && p.next_poll_at_ms >= 0
                    && (INITIAL_POLL_SECONDS..=MAX_POLL_SECONDS).contains(&p.interval_seconds)
            }
            Credential::Ready(r) => {
                token_form(r.access_token.expose()).is_ok()
                    && token_form(r.refresh_token.expose()).is_ok()
                    && r.expires_at_ms >= 0
                    && text(&r.account, MAX_ID_BYTES)
                    && text(&r.handle, MAX_ACTOR_BYTES)
            }
            Credential::ReauthRequired => true,
        }
    }

    /// The stored record: the bytes of a secret are read here, for the disk encoding only.
    fn to_value(&self) -> Value {
        let mut m = Mapping::new();
        let mut put = |k: &str, v: Value| {
            m.insert(Value::from(k), v);
        };
        put(STATE, Value::from(self.state()));
        match self {
            Credential::Pending(p) => {
                put("machine_name", Value::from(p.machine_name.as_str()));
                put("device_code", Value::from(p.device_code.expose()));
                put("user_code", Value::from(p.user_code.as_str()));
                put("verification_uri", Value::from(p.verification_uri.as_str()));
                put("expires_at_ms", Value::from(p.expires_at_ms));
                put("next_poll_at_ms", Value::from(p.next_poll_at_ms));
                put("interval_seconds", Value::from(p.interval_seconds));
            }
            Credential::Ready(r) => {
                put("access_token", Value::from(r.access_token.expose()));
                put("refresh_token", Value::from(r.refresh_token.expose()));
                put("expires_at_ms", Value::from(r.expires_at_ms));
                put("account", Value::from(r.account.as_str()));
                put("handle", Value::from(r.handle.as_str()));
            }
            Credential::ReauthRequired => {}
        }
        Value::Mapping(m)
    }
}

/// The credential file: each install's credential, by canonical origin. Debug names only the
/// origins and their states.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CredentialFile {
    installs: BTreeMap<Origin, Credential>,
}

impl CredentialFile {
    /// The credential of `install`; `None` is signed out.
    pub fn get(&self, install: &Origin) -> Option<&Credential> {
        self.installs.get(install)
    }

    /// Sets the credential of `install`, or removes it with `None`.
    pub fn set(&mut self, install: Origin, credential: Option<Credential>) {
        match credential {
            Some(c) => {
                self.installs.insert(install, c);
            }
            None => {
                self.installs.remove(&install);
            }
        }
    }

    /// The installs that have a credential, sorted by origin.
    pub fn origins(&self) -> impl Iterator<Item = &Origin> {
        self.installs.keys()
    }

    pub fn len(&self) -> usize {
        self.installs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.installs.is_empty()
    }
}

impl fmt::Debug for CredentialFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.installs.iter().map(|(o, c)| (o, c.state())))
            .finish()
    }
}

/// The credential file's bytes. Empty or comment-only bytes are an empty file; an explicit null
/// document is not. A syntax error, a duplicate or non-canonical origin, an alias, a tagged key,
/// an explicit null, a missing field, a field of another state,
/// a wrong state and a bad token, code or URI form are all the one `E_CONFIG` text; an unknown
/// key is named only by its nearest known field; over the bounds or 32 installs is `E_LIMIT`. No
/// error quotes the input.
pub fn parse_credentials(bytes: &[u8]) -> Result<CredentialFile, RdyError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(config_limit(MAX_CONFIG_BYTES, LimitUnit::Bytes));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| credentials_invalid())?;
    let top = match parse_explicit(text).map_err(|r| r.into_error(credentials_invalid))? {
        (Value::Null, false) => return Ok(CredentialFile::default()),
        (Value::Mapping(m), _) => m,
        _ => return Err(credentials_invalid()),
    };
    let mut installs = None;
    for (k, v) in top {
        match k.as_str() {
            Some("installs") => installs = Some(v),
            Some(other) => return Err(unknown_credential(nearest(other, &TOP_FIELDS))),
            None => return Err(credentials_invalid()),
        }
    }
    let mut file = CredentialFile::default();
    let records = match installs {
        None => return Ok(file),
        Some(Value::Mapping(m)) => m,
        Some(_) => return Err(credentials_invalid()),
    };
    if records.len() > MAX_INSTALLS {
        return Err(config_limit(MAX_INSTALLS, LimitUnit::Installs));
    }
    for (k, v) in records {
        let origin = k
            .as_str()
            .and_then(canonical_origin)
            .ok_or_else(credentials_invalid)?;
        let Value::Mapping(record) = v else {
            return Err(credentials_invalid());
        };
        let credential = parse_record(&record)?;
        if !credential.valid(&origin) {
            return Err(credentials_invalid());
        }
        file.installs.insert(origin, credential);
    }
    Ok(file)
}

/// One record by its state: every key known for that state, none missing.
fn parse_record(m: &Mapping) -> Result<Credential, RdyError> {
    let fields: &[&str] = match m.get(STATE).and_then(Value::as_str) {
        Some("pending") => &PENDING_FIELDS,
        Some("ready") => &READY_FIELDS,
        Some("reauth_required") => &REAUTH_FIELDS,
        _ => return Err(credentials_invalid()),
    };
    for k in m.keys() {
        let key = k.as_str().ok_or_else(credentials_invalid)?;
        if fields.contains(&key) {
            continue;
        }
        let known_elsewhere = PENDING_FIELDS
            .iter()
            .chain(&READY_FIELDS)
            .any(|f| *f == key);
        if known_elsewhere {
            return Err(credentials_invalid());
        }
        return Err(unknown_credential(nearest(key, fields)));
    }
    let text = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(credentials_invalid)
    };
    let int = |k: &str| {
        m.get(k)
            .and_then(Value::as_i64)
            .ok_or_else(credentials_invalid)
    };
    Ok(match m.get(STATE).and_then(Value::as_str) {
        Some("pending") => Credential::Pending(PendingSignIn {
            machine_name: text("machine_name")?,
            device_code: Secret::new(text("device_code")?),
            user_code: text("user_code")?,
            verification_uri: text("verification_uri")?,
            expires_at_ms: int("expires_at_ms")?,
            next_poll_at_ms: int("next_poll_at_ms")?,
            interval_seconds: m
                .get("interval_seconds")
                .and_then(Value::as_u64)
                .ok_or_else(credentials_invalid)?,
        }),
        Some("ready") => Credential::Ready(ReadyCredential {
            access_token: Secret::new(text("access_token")?),
            refresh_token: Secret::new(text("refresh_token")?),
            expires_at_ms: int("expires_at_ms")?,
            account: text("account")?,
            handle: text("handle")?,
        }),
        _ => Credential::ReauthRequired,
    })
}

/// The credential file as bytes, origins sorted; the one place a token's or device code's bytes
/// are written. A credential whose fields do not have their form is the `E_CONFIG` text; over 32
/// installs or the byte bound is `E_LIMIT`.
pub fn encode_credentials(file: &CredentialFile) -> Result<Vec<u8>, RdyError> {
    if file.installs.len() > MAX_INSTALLS {
        return Err(config_limit(MAX_INSTALLS, LimitUnit::Installs));
    }
    let mut installs = Mapping::new();
    for (origin, c) in &file.installs {
        if !c.valid(origin) {
            return Err(credentials_invalid());
        }
        installs.insert(Value::from(origin.as_str()), c.to_value());
    }
    let mut top = Mapping::new();
    top.insert(Value::from("installs"), Value::Mapping(installs));
    document_bounds(&top)?;
    encode_bounded(&top, credentials_invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsa3_encoding_refuses_before_its_buffer_passes_the_bound() {
        // 256 entries of 16384 bytes: inside the entry, depth and scalar bounds, 4 MiB encoded
        let mut m = Mapping::new();
        for i in 0..256 {
            m.insert(
                Value::from(format!("k{i:03}")),
                Value::from("x".repeat(16384)),
            );
        }
        let mut out = BoundedBuf::new(MAX_CONFIG_BYTES);
        let e = write_bounded(&m, &mut out, || settings_invalid(None)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "config input exceeds 1048576 bytes",
                CONFIG_LIMIT_FIX
            )
        );
        assert!(
            out.buf.capacity() <= MAX_CONFIG_BYTES,
            "the output buffer grew to {} bytes before the refusal",
            out.buf.capacity()
        );
        // the public encoders go through it
        assert_eq!(encode_settings(&m).unwrap_err().code, E_LIMIT);
        // within the bound it is the whole encoding
        let mut small = Mapping::new();
        small.insert(Value::from("a"), Value::from("b"));
        let mut out = BoundedBuf::new(MAX_CONFIG_BYTES);
        write_bounded(&small, &mut out, || settings_invalid(None)).unwrap();
        assert_eq!(out.buf, b"a: b\n");
        // a limit met exactly is within it
        let mut out = BoundedBuf::new(5);
        write_bounded(&small, &mut out, || settings_invalid(None)).unwrap();
        let mut out = BoundedBuf::new(4);
        let e = write_bounded(&small, &mut out, || settings_invalid(None)).unwrap_err();
        assert_eq!(e.message, "config input exceeds 4 bytes");
        assert!(out.buf.capacity() <= 4);
    }
}
