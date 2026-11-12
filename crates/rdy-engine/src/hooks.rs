//! Local hooks (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ, refined by D-DC5D1): the one shared
//! check behind the `rdy_hook` tool, the `rdy hook` commands and the host callbacks.
//!
//! Two checks, each a warning that never refuses the action it looks at:
//!
//! - The code check (`kind: code`) runs on a commit message before the commit completes. It warns
//!   with `W_HOOK_NO_CITATION` when a staged path touches code whose commits cite a decision (the
//!   decision itself, a block a decision governs, or a section holding either) and the message
//!   has no `Implements:` value. It validates the caller's checkout and HEAD first and reads the
//!   index at that HEAD, never the configured checkout's.
//! - The edit check (`kind: edit`) runs after an agent edited files. It warns with
//!   `W_HOOK_DIRECT_EDIT` when a path is a working-copy member of this project or one of its
//!   registered worktrees.
//!
//! A check that cannot finish says so: `checked: false` and a `W_HOOK_UNAVAILABLE` warning with
//! one fixed reason. Installing and removing hooks belong to the local host ([`HookHost`]); an
//! engine without one refuses them. Nothing here writes to the store, a working copy or Git.
use crate::code_index::{self, CodeRepo};
use crate::engine::Engine;
use crate::error::*;
use crate::query::governing_ids;
use crate::wc::ignore::Ignore;
use crate::wc::layout::{Membership, membership_of};
use crate::wc::state::{Place, WORKTREES_DIR, WcState, worktrees};
use crate::{ReadState, Scope};
use rdy_graph::{GoverningCache, GoverningStatuses};
use rdy_model::BlockId;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The most bytes one hook request, one host's stdin or one settings file may hold.
pub const MAX_HOOK_INPUT: usize = 1_048_576;
/// The most paths one check may name.
pub const MAX_HOOK_PATHS: usize = 256;
/// The longest path one check may name, in bytes.
pub const MAX_HOOK_PATH: usize = 4096;
/// The longest commit message the code check reads, in bytes.
pub const MAX_HOOK_MESSAGE: usize = 65_536;
/// The most `Implements:` values one commit message may hold.
pub const MAX_HOOK_CITATIONS: usize = 128;
/// The longest `checkout`, in bytes.
pub const MAX_HOOK_CHECKOUT: usize = 4096;
/// The longest non-null `head`, in bytes.
pub const MAX_HOOK_HEAD: usize = 64;
/// The most paths one warning lists; the rest are counted in `omitted`.
pub const MAX_SHOWN_PATHS: usize = 20;
/// The largest Git hook launcher, in bytes; a larger existing file is refused unread.
pub const MAX_LAUNCHER_BYTES: usize = 16_384;
/// How long one host run, and the installer lock, may take.
pub const HOOK_DEADLINE: Duration = Duration::from_secs(2);
/// Every top-level key of a hook request.
pub const HOOK_KEYS: [&str; 9] = [
    "op", "kind", "repo", "checkout", "head", "paths", "message", "harness", "worktree",
];
/// The one edit-hook harness so far (D-KG0V6).
pub const CLAUDE_CODE: &str = "claude-code";

// ---------------------------------------------------------------------------------------------
// The run context
// ---------------------------------------------------------------------------------------------

/// Why a run stopped before it finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Its cancellation signal fired (a lost hook connection, an exiting worker).
    Cancelled,
    /// Its deadline passed.
    Deadline,
}

/// What a waiting Git run hears: an output reader finished (`over`: past its bound or failed),
/// or the run context woke it.
pub(crate) enum Signal {
    Read { over: bool },
    Wake,
}

/// The shared cancellation state of one run.
#[derive(Default)]
struct Cancel {
    fired: AtomicBool,
    /// The waiting Git run's channel, while one waits. A check runs one Git child at a time.
    waiting: Mutex<Option<mpsc::Sender<Signal>>>,
}

/// One hook run's deadline and cancellation signal. One context passes from the local
/// collector through ctl dispatch and the shared check to the Git executor: no Git command
/// starts once it has stopped, and a running one is killed, reaped and its output readers
/// joined before the call returns. Clones share the signal. A new context has no deadline and
/// stops only when [`RunContext::cancel`] fires.
#[derive(Clone, Default)]
pub struct RunContext {
    deadline: Option<Instant>,
    cancel: Arc<Cancel>,
    spawned: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("deadline", &self.deadline)
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl RunContext {
    /// A context with no deadline, not cancelled.
    pub fn new() -> RunContext {
        RunContext::default()
    }

    /// This context with the deadline `at`.
    pub fn with_deadline(mut self, at: Instant) -> RunContext {
        self.deadline = Some(at);
        self
    }

    /// This context with `observer` called each time a Git child has started, before the run
    /// waits on it: a test holds the child there (a barrier) and fires the cancellation or the
    /// deadline from another thread.
    pub fn on_spawn(mut self, observer: impl Fn() + Send + Sync + 'static) -> RunContext {
        self.spawned = Some(Arc::new(observer));
        self
    }

    /// The deadline, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Fires the cancellation signal and wakes the Git run waiting on this context, if any.
    pub fn cancel(&self) {
        self.cancel.fired.store(true, Ordering::SeqCst);
        let waiting = self
            .cancel
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = waiting.as_ref() {
            let _ = tx.send(Signal::Wake);
        }
    }

    /// Whether the cancellation signal fired.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.fired.load(Ordering::SeqCst)
    }

    /// Why the run must stop now, if it must: a cancellation first, then a passed deadline.
    pub fn stopped(&self) -> Option<Stop> {
        if self.is_cancelled() {
            Some(Stop::Cancelled)
        } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Some(Stop::Deadline)
        } else {
            None
        }
    }

    /// The Git run waiting on `tx` hears this context's cancellation.
    pub(crate) fn watch(&self, tx: mpsc::Sender<Signal>) {
        *self
            .cancel
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    /// The Git run no longer waits.
    pub(crate) fn unwatch(&self) {
        *self
            .cancel
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A Git child has started.
    pub(crate) fn spawned(&self) {
        if let Some(f) = &self.spawned {
            f();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

/// Which hook a request is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Code,
    Edit,
}

impl HookKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HookKind::Code => "code",
            HookKind::Edit => "edit",
        }
    }
}

/// An edit-hook harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Harness {
    #[serde(rename = "claude-code")]
    ClaudeCode,
}

/// The code check's input: the caller's checkout, its HEAD, the staged paths and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeCheck {
    /// A `code_repos` name with a local `code_paths` checkout.
    pub repo: String,
    /// The native absolute root of the caller's Git worktree.
    pub checkout: String,
    /// The worktree's full HEAD commit hash; `None` for an unborn HEAD.
    pub head: Option<String>,
    /// Repo-relative logical paths: deleted paths and both sides of renames included.
    pub paths: Vec<String>,
    /// The commit message.
    pub message: String,
}

/// The edit check's input: native absolute paths an agent edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditCheck {
    pub paths: Vec<String>,
}

/// What an install or remove targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookTarget {
    /// The `commit-msg` hook of a linked code repo's checkout.
    Code { repo: String },
    /// A harness's project-local settings, in the main project root or a registered worktree.
    Edit {
        harness: Harness,
        worktree: Option<String>,
    },
}

impl HookTarget {
    pub fn kind(&self) -> HookKind {
        match self {
            HookTarget::Code { .. } => HookKind::Code,
            HookTarget::Edit { .. } => HookKind::Edit,
        }
    }
}

/// One documented hook form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookRequest {
    CheckCode(CodeCheck),
    CheckEdit(EditCheck),
    Install(HookTarget),
    Remove(HookTarget),
}

/// `E_BAD_INPUT`: the request is no documented form, for `reason`.
pub fn form_error(reason: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        HOOK_FORM_MESSAGE.replace("<reason>", &reason.to_string()),
        HOOK_FORM_FIX,
    )
}

/// `E_LIMIT`: the hook input exceeds `limit` `unit`.
pub fn limit(limit: usize, unit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        HOOK_LIMIT_MESSAGE
            .replace("<limit>", &limit.to_string())
            .replace("<unit>", unit),
        HOOK_LIMIT_FIX,
    )
}

/// `E_BAD_INPUT`: this host has no local hooks to install or remove.
pub fn host_absent() -> RdyError {
    RdyError::new(E_BAD_INPUT, HOOK_HOST_ABSENT_MESSAGE, HOOK_HOST_ABSENT_FIX)
}

/// Counts bytes up to a bound, failing past it: the size of a value's JSON without keeping it.
struct Bounded {
    left: usize,
}

impl std::io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.left {
            self.left = 0;
            return Err(std::io::Error::other("over"));
        }
        self.left -= buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The fields each form takes, required first: `(op, kind) → (required, optional)`.
fn form_fields(op: &str, kind: HookKind) -> (&'static [&'static str], &'static [&'static str]) {
    match (op, kind) {
        ("check", HookKind::Code) => (&["repo", "checkout", "head", "paths", "message"], &[]),
        ("check", HookKind::Edit) => (&["paths"], &[]),
        (_, HookKind::Code) => (&["repo"], &[]),
        (_, HookKind::Edit) => (&["harness"], &["worktree"]),
    }
}

impl HookRequest {
    /// Parses a request from its JSON bytes; more than [`MAX_HOOK_INPUT`] bytes is refused
    /// before any is parsed.
    pub fn from_slice(bytes: &[u8]) -> Result<HookRequest, RdyError> {
        if bytes.len() > MAX_HOOK_INPUT {
            return Err(limit(MAX_HOOK_INPUT, "bytes"));
        }
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| form_error(format!("the input is not JSON ({e})")))?;
        HookRequest::from_value(&v)
    }

    /// Parses a request from a JSON value: one closed form of [`HOOK_KEYS`]. Its JSON may hold
    /// at most [`MAX_HOOK_INPUT`] bytes. An unknown key names the nearest known one; a key of
    /// another form, a missing key and a wrong type each refuse with `E_BAD_INPUT`; a bound
    /// passed refuses with `E_LIMIT`. A null value is an absent key, except `head`, where null
    /// names an unborn HEAD.
    pub fn from_value(v: &Value) -> Result<HookRequest, RdyError> {
        if serde_json::to_writer(
            Bounded {
                left: MAX_HOOK_INPUT,
            },
            v,
        )
        .is_err()
        {
            return Err(limit(MAX_HOOK_INPUT, "bytes"));
        }
        let Some(map) = v.as_object() else {
            return Err(form_error("the input is not a JSON object"));
        };
        for key in map.keys() {
            if !HOOK_KEYS.contains(&key.as_str()) {
                let hint = match crate::did_you_mean::nearest(key, &HOOK_KEYS) {
                    Some(near) => format!(" (did you mean \"{near}\"?)"),
                    None => format!(" (the fields are {})", HOOK_KEYS.join(", ")),
                };
                return Err(form_error(format!("unknown field \"{key}\"{hint}")));
            }
        }
        let given = |k: &str| map.get(k).filter(|v| !v.is_null() || k == "head");
        let op = match given("op") {
            None => return Err(form_error("pass op")),
            Some(Value::String(s)) if matches!(s.as_str(), "check" | "install" | "remove") => {
                s.as_str()
            }
            Some(_) => return Err(form_error("op is check, install or remove")),
        };
        let kind = match given("kind") {
            None => return Err(form_error("pass kind")),
            Some(Value::String(s)) if s == "code" => HookKind::Code,
            Some(Value::String(s)) if s == "edit" => HookKind::Edit,
            Some(_) => return Err(form_error("kind is code or edit")),
        };
        let (required, optional) = form_fields(op, kind);
        for key in HOOK_KEYS.iter().skip(2) {
            if given(key).is_some() && !required.contains(key) && !optional.contains(key) {
                return Err(form_error(format!(
                    "remove {key}: the {op} {} form does not take it",
                    kind.as_str()
                )));
            }
        }
        for key in required {
            if given(key).is_none() {
                return Err(form_error(format!("pass {key}")));
            }
        }
        match (op, kind) {
            ("check", HookKind::Code) => Ok(HookRequest::CheckCode(code_check(map)?)),
            ("check", HookKind::Edit) => Ok(HookRequest::CheckEdit(EditCheck {
                paths: paths(map, edit_path)?,
            })),
            (op, kind) => {
                let target = match kind {
                    HookKind::Code => HookTarget::Code { repo: repo(map)? },
                    HookKind::Edit => HookTarget::Edit {
                        harness: match map.get("harness") {
                            Some(Value::String(s)) if s == CLAUDE_CODE => Harness::ClaudeCode,
                            _ => return Err(form_error(format!("harness is {CLAUDE_CODE}"))),
                        },
                        worktree: match map.get("worktree") {
                            None | Some(Value::Null) => None,
                            Some(Value::String(s)) if plain_name(s) => Some(s.clone()),
                            Some(_) => {
                                return Err(form_error(
                                    "worktree is the name of a registered worktree",
                                ));
                            }
                        },
                    },
                };
                Ok(if op == "install" {
                    HookRequest::Install(target)
                } else {
                    HookRequest::Remove(target)
                })
            }
        }
    }
}

/// One plain name: no separator, no `.` or `..`, not empty.
fn plain_name(s: &str) -> bool {
    let mut c = Path::new(s).components();
    !s.contains(['/', '\\', '\0'])
        && matches!(c.next(), Some(Component::Normal(_)))
        && c.next().is_none()
}

fn string_field<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str, RdyError> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(form_error(format!("{key} is a string"))),
    }
}

fn repo(map: &Map<String, Value>) -> Result<String, RdyError> {
    let repo = string_field(map, "repo")?;
    if repo.is_empty() {
        return Err(form_error("repo names a code_repos entry"));
    }
    Ok(repo.to_string())
}

fn code_check(map: &Map<String, Value>) -> Result<CodeCheck, RdyError> {
    let repo = repo(map)?;
    let checkout = string_field(map, "checkout")?;
    if checkout.len() > MAX_HOOK_CHECKOUT {
        return Err(limit(MAX_HOOK_CHECKOUT, "checkout bytes"));
    }
    if checkout.contains('\0') || !Path::new(checkout).is_absolute() || dotted(checkout) {
        return Err(form_error(
            "checkout is the absolute native root of a Git worktree",
        ));
    }
    let head = match map.get("head") {
        Some(Value::Null) => None,
        Some(Value::String(h)) => {
            if h.len() > MAX_HOOK_HEAD {
                return Err(limit(MAX_HOOK_HEAD, "head bytes"));
            }
            if !code_index::is_hash(h.as_bytes()) {
                return Err(form_error(
                    "head is a full lowercase commit hash, or null for an unborn HEAD",
                ));
            }
            Some(h.clone())
        }
        _ => {
            return Err(form_error(
                "head is a full lowercase commit hash, or null for an unborn HEAD",
            ));
        }
    };
    let paths = paths(map, code_path)?;
    let message = string_field(map, "message")?;
    if message.len() > MAX_HOOK_MESSAGE {
        return Err(limit(MAX_HOOK_MESSAGE, "message bytes"));
    }
    Ok(CodeCheck {
        repo,
        checkout: checkout.to_string(),
        head,
        paths,
        message: message.to_string(),
    })
}

/// Whether a native path holds a `.` or `..` segment.
fn dotted(p: &str) -> bool {
    p.split(['/', '\\']).any(|s| s == "." || s == "..")
}

/// A code path: repo-relative and logical (`/`-separated plain segments).
fn code_path(p: &str) -> Result<(), RdyError> {
    if p.split('/').any(|s| s.is_empty() || s == "." || s == "..") || p.contains('\0') {
        return Err(form_error(format!(
            "{p:?} is not a repo-relative code path"
        )));
    }
    Ok(())
}

/// An edit path: native and absolute, with no `.` or `..` segment.
fn edit_path(p: &str) -> Result<(), RdyError> {
    if p.contains('\0') || !Path::new(p).is_absolute() || dotted(p) {
        return Err(form_error(format!("{p:?} is not an absolute native path")));
    }
    Ok(())
}

/// The `paths` list: at most [`MAX_HOOK_PATHS`] strings of at most [`MAX_HOOK_PATH`] bytes
/// each, each passing `check`. Both bounds are checked before a path is copied.
fn paths(
    map: &Map<String, Value>,
    check: fn(&str) -> Result<(), RdyError>,
) -> Result<Vec<String>, RdyError> {
    let Some(Value::Array(items)) = map.get("paths") else {
        return Err(form_error("paths is a list of strings"));
    };
    if items.len() > MAX_HOOK_PATHS {
        return Err(limit(MAX_HOOK_PATHS, "paths"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Value::String(p) = item else {
            return Err(form_error("paths is a list of strings"));
        };
        if p.len() > MAX_HOOK_PATH {
            return Err(limit(MAX_HOOK_PATH, "path bytes"));
        }
        check(p)?;
        out.push(p.clone());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------------------------

/// Why a check could not finish: the fixed reasons of `W_HOOK_UNAVAILABLE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Unavailable {
    #[serde(rename = "project unavailable")]
    Project,
    #[serde(rename = "checkout unavailable")]
    Checkout,
    #[serde(rename = "history unavailable")]
    History,
    #[serde(rename = "unsafe path")]
    UnsafePath,
    #[serde(rename = "invalid event")]
    InvalidEvent,
    #[serde(rename = "input limit")]
    InputLimit,
    #[serde(rename = "deadline")]
    Deadline,
    #[serde(rename = "I/O failure")]
    Io,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unavailable::Project => "project unavailable",
            Unavailable::Checkout => "checkout unavailable",
            Unavailable::History => "history unavailable",
            Unavailable::UnsafePath => "unsafe path",
            Unavailable::InvalidEvent => "invalid event",
            Unavailable::InputLimit => "input limit",
            Unavailable::Deadline => "deadline",
            Unavailable::Io => "I/O failure",
        }
    }
}

/// Why an install or remove left a file as it was: the fixed reasons of `W_HOOK_CONFLICT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Conflict {
    #[serde(rename = "existing hook")]
    ExistingHook,
    #[serde(rename = "changed owned file")]
    ChangedOwnedFile,
    #[serde(rename = "external hooks path")]
    ExternalHooksPath,
    #[serde(rename = "invalid settings")]
    InvalidSettings,
    #[serde(rename = "unsafe path")]
    UnsafePath,
    #[serde(rename = "file changed")]
    FileChanged,
    #[serde(rename = "installer busy")]
    InstallerBusy,
}

impl Conflict {
    pub fn as_str(self) -> &'static str {
        match self {
            Conflict::ExistingHook => "existing hook",
            Conflict::ChangedOwnedFile => "changed owned file",
            Conflict::ExternalHooksPath => "external hooks path",
            Conflict::InvalidSettings => "invalid settings",
            Conflict::UnsafePath => "unsafe path",
            Conflict::FileChanged => "file changed",
            Conflict::InstallerBusy => "installer busy",
        }
    }
}

/// The underlying error behind an unavailable check: its code and message, never a file body,
/// token or host event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookDiagnostic {
    pub code: &'static str,
    pub message: String,
}

/// One hook warning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookWarning {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// The fixed reason of an unavailable check or a conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// The paths it is about, at most [`MAX_SHOWN_PATHS`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// How many more paths it is about.
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<HookDiagnostic>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl HookWarning {
    fn listing(code: &'static str, message: &str, fix: &str, mut paths: Vec<String>) -> Self {
        let omitted = paths.len().saturating_sub(MAX_SHOWN_PATHS);
        paths.truncate(MAX_SHOWN_PATHS);
        HookWarning {
            code,
            message: message.to_string(),
            fix: fix.to_string(),
            reason: None,
            paths,
            omitted,
            diagnostic: None,
        }
    }

    /// `W_HOOK_DIRECT_EDIT` about `paths`.
    pub fn direct_edit(paths: Vec<String>) -> Self {
        HookWarning::listing(
            W_HOOK_DIRECT_EDIT,
            HOOK_DIRECT_EDIT_MESSAGE,
            HOOK_DIRECT_EDIT_FIX,
            paths,
        )
    }

    /// `W_HOOK_NO_CITATION` about the anchored `paths`.
    pub fn no_citation(paths: Vec<String>) -> Self {
        HookWarning::listing(
            W_HOOK_NO_CITATION,
            HOOK_NO_CITATION_MESSAGE,
            HOOK_NO_CITATION_FIX,
            paths,
        )
    }

    /// `W_HOOK_UNAVAILABLE` for `reason`, with the underlying error when there is one.
    pub fn unavailable(reason: Unavailable, cause: Option<&RdyError>) -> Self {
        HookWarning {
            code: W_HOOK_UNAVAILABLE,
            message: HOOK_UNAVAILABLE_MESSAGE.replace("<reason>", reason.as_str()),
            fix: HOOK_UNAVAILABLE_FIX.to_string(),
            reason: Some(reason.as_str()),
            paths: vec![],
            omitted: 0,
            diagnostic: cause.map(|e| HookDiagnostic {
                code: e.code,
                message: e.message.clone(),
            }),
        }
    }

    /// `W_HOOK_CONFLICT`: `path` was left as it was, for `reason`.
    pub fn conflict(path: &str, reason: Conflict) -> Self {
        HookWarning {
            code: W_HOOK_CONFLICT,
            message: HOOK_CONFLICT_MESSAGE
                .replace("<path>", path)
                .replace("<reason>", reason.as_str()),
            fix: HOOK_CONFLICT_FIX.to_string(),
            reason: Some(reason.as_str()),
            paths: vec![path.to_string()],
            omitted: 0,
            diagnostic: None,
        }
    }
}

/// A check's result. `checked: false` means the evidence is incomplete: a warning says why, and
/// the result is never a clean one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookCheck {
    pub kind: HookKind,
    pub checked: bool,
    pub warnings: Vec<HookWarning>,
}

impl HookCheck {
    fn clean(kind: HookKind) -> HookCheck {
        HookCheck {
            kind,
            checked: true,
            warnings: vec![],
        }
    }

    fn unavailable(kind: HookKind, reason: Unavailable, cause: Option<&RdyError>) -> HookCheck {
        HookCheck {
            kind,
            checked: false,
            warnings: vec![HookWarning::unavailable(reason, cause)],
        }
    }
}

/// What an install or remove did to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileResult {
    Written,
    Kept,
    Removed,
    Skipped,
}

/// One file an install or remove looked at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookFile {
    pub path: String,
    pub result: FileResult,
}

/// An install's or remove's result: every file it looked at, and a warning for each skipped or
/// incomplete step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookInstall {
    pub kind: HookKind,
    pub files: Vec<HookFile>,
    pub warnings: Vec<HookWarning>,
}

/// The local host that installs and removes hooks (an input seam like
/// [`crate::instructions::PersonHost`]): it knows its binary and the project root, and takes no
/// executable, shell command or directory from a request. An engine with none refuses both
/// ([`host_absent`]); a check needs no host.
pub trait HookHost: Send + Sync {
    fn install(&self, target: &HookTarget, ctx: &RunContext) -> Result<HookInstall, RdyError>;
    fn remove(&self, target: &HookTarget, ctx: &RunContext) -> Result<HookInstall, RdyError>;
}

/// Runs one request on `engine`: a check through the shared check, an install or remove
/// through the engine's [`HookHost`] (none refuses). The caller has applied its session and
/// role rules (readers check; installing and removing need a writer or the person's local
/// session).
pub fn run(engine: &dyn Engine, req: &HookRequest, ctx: &RunContext) -> Result<Value, RdyError> {
    Ok(match req {
        HookRequest::CheckCode(c) => to_json(&check_code(&engine.read(), c, ctx)?),
        HookRequest::CheckEdit(c) => to_json(&check_edit(&engine.read(), c, ctx)),
        HookRequest::Install(t) => to_json(
            &engine
                .hook_host()
                .ok_or_else(host_absent)?
                .install(t, ctx)?,
        ),
        HookRequest::Remove(t) => {
            to_json(&engine.hook_host().ok_or_else(host_absent)?.remove(t, ctx)?)
        }
    })
}

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// The code check
// ---------------------------------------------------------------------------------------------

/// The unavailable result of a Git read that failed: a stop of the run context reports the
/// deadline, any other failure `reason`.
fn git_unavailable(ctx: &RunContext, reason: Unavailable, e: &RdyError) -> HookCheck {
    match ctx.stopped() {
        Some(_) => HookCheck::unavailable(HookKind::Code, Unavailable::Deadline, Some(e)),
        None => HookCheck::unavailable(HookKind::Code, reason, Some(e)),
    }
}

/// The code check (D-DC5D1): warns when one of `req.paths` touches code anchored to a decision
/// at the caller's HEAD and `req.message` has no `Implements:` value. The caller's checkout must
/// be the configured checkout of `req.repo` or a registered Git worktree of it (else `checkout
/// unavailable`), and its HEAD must still be `req.head` (else `history unavailable`). A verified
/// unborn HEAD, or a history with no anchor on the paths, is clean. `E_BAD_INPUT` only for a
/// repo name that is not linked; every other failure is `checked: false`.
pub fn check_code(
    st: &ReadState,
    req: &CodeCheck,
    ctx: &RunContext,
) -> Result<HookCheck, RdyError> {
    let kind = HookKind::Code;
    if ctx.stopped().is_some() {
        return Ok(HookCheck::unavailable(kind, Unavailable::Deadline, None));
    }
    let user = crate::config_levels::user_file_from_env();
    let linked = match crate::config_levels::linked_repos(st.root(), user.as_deref()) {
        Ok((linked, _warnings)) => linked,
        Err(e) => return Ok(HookCheck::unavailable(kind, Unavailable::Project, Some(&e))),
    };
    let Some(entry) = linked.iter().find(|l| l.name == req.repo) else {
        let names: Vec<&str> = linked.iter().map(|l| l.name.as_str()).collect();
        let hint = match crate::did_you_mean::nearest(&req.repo, &names) {
            Some(near) => format!(" (did you mean \"{near}\"?)"),
            None => String::new(),
        };
        return Err(form_error(format!(
            "repo \"{}\" is no code_repos entry with a local checkout{hint}",
            req.repo
        )));
    };
    let checkout_failed = |e: std::io::Error| {
        let e = RdyError::new(E_IO, e.to_string(), "check the checkout");
        HookCheck::unavailable(kind, Unavailable::Checkout, Some(&e))
    };
    let configured = match std::fs::canonicalize(&entry.path) {
        Ok(root) => CodeRepo {
            configured: entry.name.clone(),
            root,
        },
        Err(e) => return Ok(checkout_failed(e)),
    };
    let checkout = match std::fs::canonicalize(&req.checkout) {
        Ok(root) => CodeRepo {
            configured: entry.name.clone(),
            root,
        },
        Err(e) => return Ok(checkout_failed(e)),
    };
    match code_index::same_repository(&configured, &checkout, Some(ctx)) {
        Ok(true) => {}
        Ok(false) => return Ok(HookCheck::unavailable(kind, Unavailable::Checkout, None)),
        Err(e) => return Ok(git_unavailable(ctx, Unavailable::Checkout, &e)),
    }
    let head = match code_index::head_of_in(&checkout, Some(ctx)) {
        Ok(head) => head,
        Err(e) => return Ok(git_unavailable(ctx, Unavailable::History, &e)),
    };
    // byte for byte: the HEAD the caller collected under is the HEAD it has now
    if head != req.head {
        return Ok(HookCheck::unavailable(kind, Unavailable::History, None));
    }
    let Some(head) = head else {
        return Ok(HookCheck::clean(kind));
    };
    let index = code_index::read_tree_in(&checkout, &head, Some(ctx)).and_then(|tree| {
        let at_head: BTreeSet<String> = tree.into_keys().collect();
        code_index::index_at_in(&checkout, &head, &at_head, Some(ctx))
    });
    let index = match index {
        Ok(index) => index,
        Err(e) => return Ok(git_unavailable(ctx, Unavailable::History, &e)),
    };
    let mut anchors = Anchors::new(st);
    let anchored: Vec<String> = req
        .paths
        .iter()
        .filter(|p| {
            index
                .files
                .get(p.as_str())
                .is_some_and(|cited| cited.keys().any(|text| anchors.reaches_decision(text)))
        })
        .cloned()
        .collect();
    if anchored.is_empty() {
        return Ok(HookCheck::clean(kind));
    }
    let cites =
        match code_index::message_citations(&checkout, &req.message, MAX_HOOK_CITATIONS, Some(ctx))
        {
            Ok(cites) => cites,
            Err(e) if e.code == E_LIMIT => return Err(e),
            Err(e) => return Ok(git_unavailable(ctx, Unavailable::Io, &e)),
        };
    if !cites.is_empty() {
        return Ok(HookCheck::clean(kind));
    }
    Ok(HookCheck {
        kind,
        checked: true,
        warnings: vec![HookWarning::no_citation(anchored)],
    })
}

/// Which cited texts lead to a decision, read once each: the text names a block (in canon, else
/// among the in-flight changes, so a still-proposed decision counts), and that block or one under
/// it is a decision, or is governed by one, whatever the decision's status. A `reflects` match
/// is no citation and plays no part.
struct Anchors<'a> {
    st: &'a ReadState,
    seen: BTreeMap<String, bool>,
}

impl<'a> Anchors<'a> {
    fn new(st: &'a ReadState) -> Self {
        Anchors {
            st,
            seen: BTreeMap::new(),
        }
    }

    fn reaches_decision(&mut self, text: &str) -> bool {
        if let Some(found) = self.seen.get(text) {
            return *found;
        }
        let found = [Scope::Canon, Scope::Inflight]
            .iter()
            .find_map(|scope| {
                let id = self.st.resolve_id(scope, text).ok()?;
                let m = self.st.materialized(scope).ok()?;
                m.get(&id)?;
                Some(self.leads_to_decision(scope, &id))
            })
            .unwrap_or(false);
        self.seen.insert(text.to_string(), found);
        found
    }

    /// Whether `id` or a block under it (at most `MAX_BRIEF_BLOCKS` blocks walked) is a decision
    /// or governed by one in `scope`.
    fn leads_to_decision(&self, scope: &Scope, id: &BlockId) -> bool {
        let (Ok(m), Ok(g)) = (self.st.materialized(scope), self.st.graph(scope)) else {
            return false;
        };
        let pack = self.st.pack();
        let mut cache = GoverningCache::default();
        let mut todo = vec![id.clone()];
        let mut seen: BTreeSet<BlockId> = BTreeSet::new();
        while let Some(cur) = todo.pop() {
            if !seen.insert(cur.clone()) || seen.len() > crate::brief::MAX_BRIEF_BLOCKS {
                continue;
            }
            if g.status(&cur).is_some()
                || !governing_ids(pack, &m, &g, &cur, GoverningStatuses::All, &mut cache).is_empty()
            {
                return true;
            }
            todo.extend(m.children(Some(&cur)).into_iter().map(|b| b.id.clone()));
        }
        false
    }
}

// ---------------------------------------------------------------------------------------------
// The local collector of the code hook
// ---------------------------------------------------------------------------------------------

/// What the code hook's local runner collected from the caller's checkout: its canonical root,
/// its HEAD (`None` while unborn) and the staged paths, each read under one run context. These,
/// with the message, are the code check's `checkout`, `head` and `paths`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub checkout: String,
    pub head: Option<String>,
    pub paths: Vec<String>,
}

/// The local collector of the code hook (D-DC5D1), run by the process Git started, never by the
/// daemon: the checkout that holds `cwd` (its top folder and its Git directories), its HEAD, then
/// the paths of the actual staged diff against that HEAD, read from the index Git names in
/// `index_file` (`GIT_INDEX_FILE`; relative to `cwd`) or else the checkout's own. Deleted paths
/// and both sides of a rename are included; unstaged changes play no part. The checkout's
/// identity and HEAD are read again after the diff: a changed identity is `checkout
/// unavailable` and a changed HEAD `history unavailable`. More than [`MAX_HOOK_PATHS`] paths,
/// or a path past [`MAX_HOOK_PATH`] bytes, is `input limit`; a path that is not UTF-8 is `unsafe
/// path`. Every failure is the code check's unavailable result, and a stopped `ctx` gives
/// `deadline`. Nothing here writes to Git.
pub fn collect_staged(
    cwd: &Path,
    index_file: Option<&std::ffi::OsStr>,
    ctx: &RunContext,
) -> Result<Staged, HookCheck> {
    let kind = HookKind::Code;
    if ctx.stopped().is_some() {
        return Err(HookCheck::unavailable(kind, Unavailable::Deadline, None));
    }
    let failed = |what: &str, out: &std::process::Output| {
        let err = String::from_utf8_lossy(&out.stderr);
        RdyError::new(
            E_IO,
            format!("git {what} failed: {}", err.trim()),
            "check the checkout with git status",
        )
    };
    // the caller's checkout: the top folder of the worktree that holds `cwd`
    let mut c = code_index::git_command(cwd);
    c.args(["rev-parse", "--show-toplevel"]);
    let out = code_index::run_ctx(c, code_index::Limits::GIT, Some(ctx), None)
        .map_err(|e| git_unavailable(ctx, Unavailable::Checkout, &e))?;
    if !out.status.success() {
        let e = failed("rev-parse", &out);
        return Err(HookCheck::unavailable(
            kind,
            Unavailable::Checkout,
            Some(&e),
        ));
    }
    let printed = String::from_utf8_lossy(&out.stdout);
    let printed = printed.trim_end_matches(['\n', '\r']);
    let root = std::fs::canonicalize(printed).map_err(|e| {
        let e = RdyError::new(E_IO, e.to_string(), "check the checkout");
        HookCheck::unavailable(kind, Unavailable::Checkout, Some(&e))
    })?;
    let Some(checkout) = root.to_str().map(String::from) else {
        return Err(HookCheck::unavailable(kind, Unavailable::UnsafePath, None));
    };
    let repo = CodeRepo {
        configured: checkout.clone(),
        root,
    };
    let identity = || {
        code_index::git_admin(&repo, Some(ctx))
            .map_err(|e| git_unavailable(ctx, Unavailable::Checkout, &e))
    };
    let head_now = || {
        code_index::head_of_in(&repo, Some(ctx))
            .map_err(|e| git_unavailable(ctx, Unavailable::History, &e))
    };
    let before = identity()?;
    let head = head_now()?;
    // the staged diff of the index Git names, never the daemon's or this process's guess
    let mut c = code_index::git_command(&repo.root);
    if let Some(index) = index_file {
        c.env("GIT_INDEX_FILE", cwd.join(index));
    }
    c.args([
        "diff",
        "--cached",
        "--name-only",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--ignore-submodules=none",
    ]);
    // at most the paths a check takes, each with its separator: bounded before it is kept
    let lim = code_index::Limits {
        stdout: MAX_HOOK_PATHS * (MAX_HOOK_PATH + 1),
        stderr: code_index::MAX_GIT_ERROR,
    };
    let out = code_index::run_ctx(c, lim, Some(ctx), None).map_err(|e| {
        let reason = if e.code == E_LIMIT {
            Unavailable::InputLimit
        } else {
            Unavailable::Io
        };
        git_unavailable(ctx, reason, &e)
    })?;
    if !out.status.success() {
        let e = failed("diff", &out);
        return Err(HookCheck::unavailable(kind, Unavailable::Io, Some(&e)));
    }
    let mut paths: Vec<String> = Vec::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        if paths.len() == MAX_HOOK_PATHS {
            let e = limit(MAX_HOOK_PATHS, "paths");
            return Err(HookCheck::unavailable(
                kind,
                Unavailable::InputLimit,
                Some(&e),
            ));
        }
        if raw.len() > MAX_HOOK_PATH {
            let e = limit(MAX_HOOK_PATH, "path bytes");
            return Err(HookCheck::unavailable(
                kind,
                Unavailable::InputLimit,
                Some(&e),
            ));
        }
        let Ok(p) = std::str::from_utf8(raw) else {
            return Err(HookCheck::unavailable(kind, Unavailable::UnsafePath, None));
        };
        paths.push(p.to_string());
    }
    // the checkout and its HEAD are still the ones the paths were collected under
    if identity()? != before {
        return Err(HookCheck::unavailable(kind, Unavailable::Checkout, None));
    }
    if head_now()? != head {
        return Err(HookCheck::unavailable(kind, Unavailable::History, None));
    }
    Ok(Staged {
        checkout,
        head,
        paths,
    })
}

// ---------------------------------------------------------------------------------------------
// The edit check
// ---------------------------------------------------------------------------------------------

/// One working copy the edit check knows: its canonical root and its place.
struct WcCopy {
    root: PathBuf,
    place: Place,
    /// The root's `.rdyignore` and the base's tracked paths, read on first use.
    loaded: Option<Result<(Ignore, BTreeSet<String>), RdyError>>,
}

impl WcCopy {
    fn loaded(&mut self) -> &Result<(Ignore, BTreeSet<String>), RdyError> {
        let place = &self.place;
        self.loaded.get_or_insert_with(|| {
            let ignore = Ignore::load(&place.root)?;
            let st = WcState::load(place)?;
            Ok((ignore, st.base.files.into_keys().collect()))
        })
    }
}

/// Where an edited path lies.
enum Located {
    /// Under no working copy of this project.
    Outside,
    /// Under the working copy `copy`, at the logical path `rel`.
    Inside { copy: usize, rel: String },
    /// Reached through a link or reparse point into a working copy, or one sits below a root.
    Unsafe,
}

/// The working copies of the project at `project` (canonical): the main one, then each
/// registered worktree whose recorded directory holds a pointer back to its state.
fn copies(project: &Path) -> Vec<WcCopy> {
    let mut out = vec![WcCopy {
        root: project.to_path_buf(),
        place: Place::main(project),
        loaded: None,
    }];
    for wt in worktrees(project) {
        if wt.dir.is_empty() {
            continue;
        }
        let dir = PathBuf::from(&wt.dir);
        let Some(place) = Place::from_pointer(&dir) else {
            continue;
        };
        let state = project.join(WORKTREES_DIR).join(&wt.name);
        let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !same(&place.state, &state) {
            continue;
        }
        if let Ok(root) = std::fs::canonicalize(&dir)
            && out.iter().all(|c| c.root != root)
        {
            out.push(WcCopy {
                root,
                place,
                loaded: None,
            });
        }
    }
    out
}

/// Where `path` (absolute) lies among `copies`. Above a root, links resolve as the system
/// resolves them; below it nothing is followed: a link there is [`Located::Unsafe`]. A path that
/// reaches a root only through a link elsewhere is unsafe too. The deepest root wins, so a
/// worktree inside the project is its own copy.
fn locate(path: &Path, copies: &[WcCopy]) -> Located {
    let mut cur = PathBuf::new();
    // (copy, the canonical path so far, the logical segments under the copy's root)
    let mut found: Option<(usize, PathBuf, Vec<String>)> = None;
    for c in path.components() {
        cur.push(c);
        // a Windows prefix alone (`\\?\C:`, `\\server\share`) is no directory to resolve: a
        // verbatim prefix counts as rooted, but its canonical form fails until the root follows
        if !cur.has_root() || matches!(c, Component::Prefix(_)) {
            continue;
        }
        match &mut found {
            None => match std::fs::canonicalize(&cur) {
                Ok(canon) => {
                    if let Some(i) = copies.iter().position(|k| k.root == canon) {
                        found = Some((i, canon, vec![]));
                    }
                }
                Err(_) => break,
            },
            Some((copy, canon, rel)) => {
                let Component::Normal(name) = c else {
                    return Located::Unsafe;
                };
                let Some(name) = name.to_str() else {
                    return Located::Outside;
                };
                canon.push(name);
                rel.push(name.to_string());
                match std::fs::symlink_metadata(&cur) {
                    Ok(m) if m.file_type().is_symlink() => return Located::Unsafe,
                    Ok(m) if m.is_dir() => {
                        if let Some(i) = copies.iter().position(|k| k.root == *canon) {
                            *copy = i;
                            rel.clear();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some((copy, _, rel)) = found {
        if rel.is_empty() {
            return Located::Outside;
        }
        return Located::Inside {
            copy,
            rel: rel.join("/"),
        };
    }
    // no root on the path itself: a link elsewhere may still lead into one
    let mut tail: Vec<&std::ffi::OsStr> = vec![];
    let mut at = path;
    let resolved = loop {
        if let Ok(mut p) = std::fs::canonicalize(at) {
            p.extend(tail.iter().rev());
            break Some(p);
        }
        match (at.file_name(), at.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                at = parent;
            }
            _ => break None,
        }
    };
    match resolved {
        Some(p) if copies.iter().any(|k| p.starts_with(&k.root)) => Located::Unsafe,
        _ => Located::Outside,
    }
}

/// The edit check (D-NNQ0A, D-GHR0Y): warns when one of `req.paths` is a working-copy member of
/// this project or one of its registered worktrees (tracked members, new member files, folder
/// files and managed assets). A code file, an ignored file or another project's path gets no
/// warning. Paths are classified one at a time, each alone; a link below a root, a root that
/// does not resolve or a working copy whose state cannot be read makes the result
/// `checked: false`. The deadline and cancellation are observed between paths.
pub fn check_edit(st: &ReadState, req: &EditCheck, ctx: &RunContext) -> HookCheck {
    let kind = HookKind::Edit;
    let project = match std::fs::canonicalize(st.root()) {
        Ok(p) => p,
        Err(e) => {
            let e = RdyError::new(E_IO, e.to_string(), "check the project folder");
            return HookCheck::unavailable(kind, Unavailable::Project, Some(&e));
        }
    };
    let mut copies = copies(&project);
    let mut members = vec![];
    let mut unavailable: Vec<(Unavailable, Option<RdyError>)> = vec![];
    let mut note = |reason: Unavailable, e: Option<RdyError>| {
        if unavailable.iter().all(|(r, _)| *r != reason) {
            unavailable.push((reason, e));
        }
    };
    for p in &req.paths {
        if ctx.stopped().is_some() {
            note(Unavailable::Deadline, None);
            break;
        }
        let (copy, rel) = match locate(Path::new(p), &copies) {
            Located::Outside => continue,
            Located::Unsafe => {
                note(Unavailable::UnsafePath, None);
                continue;
            }
            Located::Inside { copy, rel } => (copy, rel),
        };
        let k = &mut copies[copy];
        let root = k.root.clone();
        let (ignore, tracked) = match k.loaded() {
            Ok(l) => l,
            Err(e) => {
                note(Unavailable::Io, Some(e.clone()));
                continue;
            }
        };
        match membership_of(&root, &rel, ignore, tracked.contains(&rel)) {
            Membership::Yes => members.push(p.clone()),
            Membership::No => {}
            Membership::Unsafe => note(Unavailable::UnsafePath, None),
        }
    }
    let mut warnings = vec![];
    if !members.is_empty() {
        warnings.push(HookWarning::direct_edit(members));
    }
    let checked = unavailable.is_empty();
    for (reason, e) in &unavailable {
        warnings.push(HookWarning::unavailable(*reason, e.as_ref()));
    }
    HookCheck {
        kind,
        checked,
        warnings,
    }
}
