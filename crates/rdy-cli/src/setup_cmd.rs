//! `rdy setup` (ain5, D-8X04F; agi4, D-PHH2Q): the person's empty rules project, then the stub.
//! It creates the meta project at the configured place when nothing stands there, and writes
//! three lines into `CLAUDE.md` and `AGENTS.md` of the chosen project root. Outside a project, or
//! inside the meta project, it writes no stub. The stub goes first, before the person's own text,
//! which stays byte for byte; a file that already holds the stub is left alone. A target that is
//! not a regular file is refused before either file is written, and a link is never followed.
//! The meta project never gets rules, a copy of the guide, a policy or a stub. A failed meta step
//! is a warning: the project's stubs still go in. No harness detection, MCP registration, skill,
//! hook, download or rule import is part of it.
use crate::conn::{is_project, require_project};
use crate::instructions_cmd::discover;
use rdy_daemon::ErrorReply;
use rdy_engine::instructions::{PersonPlace, place_person};
use rdy_engine::instructions_config::{PersonPath, PersonSource, resolve_person_from_env};
use rdy_engine::{E_IO, E_NO_PROJECT, Rdy, RdyError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The stub, with its final newline.
pub(crate) const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

/// The files that take the stub, in the order they are written.
pub(crate) const TARGETS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// The fix of a target that is not a regular file.
const NOT_REGULAR_FIX: &str = "replace it with a regular file, then run rdy setup again";

/// The project `rdy setup` serves: the root that takes the stub and the project itself.
pub(crate) type Chosen = crate::instructions_cmd::Found;

/// The explicit project, else the nearest project of `cwd` (a worktree's own directory is the
/// root), else none. A project that is found must be a project; a refusal comes before any
/// write.
pub(crate) fn choose(explicit: Option<&Path>, cwd: &Path) -> Result<Option<Chosen>, ErrorReply> {
    if let Some(p) = explicit {
        require_project(p)?;
        return Ok(Some(Chosen {
            root: p.to_path_buf(),
            project: p.to_path_buf(),
        }));
    }
    match discover(cwd) {
        Some(f) => {
            require_project(&f.project)?;
            Ok(Some(f))
        }
        None => Ok(None),
    }
}

/// What one target needs.
enum Plan {
    /// No file: it is created with the stub.
    Create,
    /// A regular file without the stub: the stub goes before its bytes, and the file keeps its
    /// permissions.
    Prepend(Vec<u8>, std::fs::Permissions),
    /// A regular file that holds the stub: left alone.
    Keep,
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the file and its directory, then run rdy setup again",
    )
}

/// What `path` needs, read without following a link.
fn preflight(path: &Path) -> Result<Plan, RdyError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Plan::Create),
        Err(e) => return Err(io(path, e)),
    };
    if !meta.file_type().is_file() {
        return Err(RdyError::new(
            E_IO,
            format!("{} is not a regular instruction file", path.display()),
            NOT_REGULAR_FIX,
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| io(path, e))?;
    let has = bytes.windows(STUB.len()).any(|w| w == STUB.as_bytes());
    Ok(if has {
        Plan::Keep
    } else {
        Plan::Prepend(bytes, meta.permissions())
    })
}

/// The stub files of `root`: both targets are checked first, then each that needs it is written
/// durably. A failure after a write names what was written; a repeat completes the rest once.
fn stubs(root: &Path) -> Result<(Vec<Value>, Vec<String>), ErrorReply> {
    let paths: Vec<PathBuf> = TARGETS.iter().map(|t| root.join(t)).collect();
    let mut plans = Vec::with_capacity(paths.len());
    for p in &paths {
        plans.push(preflight(p)?);
    }
    let mut written: Vec<&Path> = vec![];
    let mut files = vec![];
    let mut text = vec![];
    for (path, plan) in paths.iter().zip(plans) {
        let (bytes, perms) = match plan {
            Plan::Keep => {
                files.push(json!({"path": path.to_string_lossy(), "result": "kept"}));
                text.push(format!("{} already has the stub", path.display()));
                continue;
            }
            Plan::Create => (STUB.as_bytes().to_vec(), None),
            Plan::Prepend(old, perms) => ([STUB.as_bytes(), &old].concat(), Some(perms)),
        };
        let wrote = rdy_engine::write_durable(path, &bytes)
            .and_then(|()| perms.map_or(Ok(()), |p| std::fs::set_permissions(path, p)));
        if let Err(e) = wrote {
            let done: Vec<String> = written
                .iter()
                .map(|p| format!("; {} was written", p.display()))
                .collect();
            return Err(RdyError::new(
                E_IO,
                format!("{}: {e}{}", path.display(), done.concat()),
                "fix the cause, then run rdy setup again; it leaves a file that has the stub alone",
            )
            .into());
        }
        written.push(path);
        files.push(json!({"path": path.to_string_lossy(), "result": "written"}));
        text.push(format!("wrote the stub to {}", path.display()));
    }
    Ok((files, text))
}

/// The init prompt (agi4): after `rdy init`, until both stubs and the meta project are in place.
pub(crate) const INIT_PROMPT: &str =
    "Run rdy setup to prepare your rules project and this project's agent instruction files.";

/// The status prompt (agi4): while the meta project is missing.
pub(crate) const STATUS_PROMPT: &str = "Your own rules project is missing. Run rdy setup.";

const OCCUPIED_FIX: &str = "Choose an unused absolute path with instructions.person, or move this path aside, then run rdy setup.";

/// What the meta step did.
struct Meta {
    path: Option<PathBuf>,
    /// `created`, `kept` or `warning`.
    result: &'static str,
    message: String,
    code: Option<String>,
    fix: Option<String>,
}

impl Meta {
    fn done(path: &Path, result: &'static str, message: String) -> Meta {
        Meta {
            path: Some(path.to_path_buf()),
            result,
            message,
            code: None,
            fix: None,
        }
    }

    fn warn(path: Option<&Path>, message: String, code: Option<&str>, fix: Option<String>) -> Meta {
        Meta {
            path: path.map(Path::to_path_buf),
            result: "warning",
            message,
            code: code.map(String::from),
            fix,
        }
    }

    fn json(&self) -> Value {
        let mut v = json!({
            "path": self.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "result": self.result,
            "message": self.message,
        });
        if let Some(c) = &self.code {
            v["code"] = json!(c);
        }
        if let Some(f) = &self.fix {
            v["fix"] = json!(f);
        }
        v
    }

    fn lines(&self) -> Vec<String> {
        let mut out = vec![self.message.clone()];
        if let Some(f) = &self.fix {
            out.push(f.clone());
        }
        out
    }
}

/// The creation warning of `path` for `e`.
fn cannot_create(path: &Path, e: &RdyError) -> Meta {
    Meta::warn(
        Some(path),
        format!(
            "Cannot create your rules project at {}: {}",
            path.display(),
            e.message
        ),
        Some(e.code),
        Some(e.fix.clone()),
    )
}

fn kept(path: &Path) -> Meta {
    Meta::done(
        path,
        "kept",
        format!("Your rules project already exists at {}.", path.display()),
    )
}

fn occupied(path: &Path) -> Meta {
    Meta::warn(
        Some(path),
        format!(
            "Cannot create your rules project at {}: the path is not an rdy project.",
            path.display()
        ),
        Some(E_NO_PROJECT),
        Some(OCCUPIED_FIX.to_string()),
    )
}

/// The meta step at `path`: an existing project is left alone, an absent path takes a fresh
/// `rdy-standard` project (closed durably, with no content), and anything else is a warning.
fn meta_at(path: &Path) -> Meta {
    if is_project(path) {
        return kept(path);
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return occupied(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            let e = RdyError::new(
                E_IO,
                format!("{}: {e}", path.display()),
                "check the path and its directory, then run rdy setup again",
            );
            return cannot_create(path, &e);
        }
    }
    match Rdy::init(path, "rdy-standard").and_then(Rdy::close) {
        Ok(()) => Meta::done(
            path,
            "created",
            format!("Created your rules project at {}.", path.display()),
        ),
        // a concurrent creator counts as kept only once the project predicate holds
        Err(_) if is_project(path) => kept(path),
        Err(e) => cannot_create(path, &e),
    }
}

/// The meta step from the user config: the configured place, or the warning that says why there
/// is none. The config's own warnings ride along.
fn meta_step() -> (Meta, Vec<Value>, Option<PathBuf>) {
    let source: PersonSource = match resolve_person_from_env() {
        Ok(s) => s,
        Err(e) => {
            return (
                Meta::warn(None, e.message, Some(e.code), Some(e.fix)),
                vec![],
                None,
            );
        }
    };
    let warnings = source
        .warnings
        .iter()
        .map(|w| serde_json::to_value(w).unwrap_or(Value::Null))
        .collect();
    match &source.path {
        PersonPath::Unavailable => (
            Meta::warn(
                None,
                rdy_engine::instructions_config::NO_HOME_MESSAGE.to_string(),
                None,
                None,
            ),
            warnings,
            None,
        ),
        PersonPath::Override(p) | PersonPath::Default(p) => (meta_at(p), warnings, Some(p.clone())),
    }
}

/// Whether `project` is the meta project at `meta`, under any spelling of either.
fn is_meta(project: &Path, meta: Option<&Path>) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    match (project.canonicalize(), meta.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `rdy setup`: the meta step, then the stubs of the chosen project (none outside a project and
/// none in the meta project). A failure in the stubs keeps the finished meta step and says so.
pub(crate) fn setup(chosen: Option<Chosen>) -> Result<(Value, String), ErrorReply> {
    let (meta, warnings, meta_path) = meta_step();
    let (files, mut text, stub) = match &chosen {
        None => (
            vec![],
            vec!["No project is open. No agent instruction files were written.".to_string()],
            json!({"result": "skipped", "reason": "outside_project"}),
        ),
        Some(c) if is_meta(&c.project, meta_path.as_deref()) => (
            vec![],
            vec![
                "This is your rules project. No agent instruction files were written.".to_string(),
            ],
            json!({"result": "skipped", "reason": "meta_project"}),
        ),
        Some(c) => {
            let (files, text) = stubs(&c.root).map_err(|mut e| {
                if meta.result == "created" {
                    e.message = format!("{}; {}", e.message, meta.message);
                }
                e
            })?;
            (files, text, json!({"result": "processed"}))
        }
    };
    let mut lines = meta.lines();
    lines.append(&mut text);
    for w in &warnings {
        lines.push(format!(
            "warning {}: {}; {}",
            w["code"].as_str().unwrap_or_default(),
            w["message"].as_str().unwrap_or_default(),
            w["fix"].as_str().unwrap_or_default()
        ));
    }
    let v = json!({"files": files, "meta": meta.json(), "stub": stub, "warnings": warnings});
    Ok((v, lines.join("\n")))
}

/// The prompt of `rdy init` in `dir` (agi4), or none: a project that is the meta project, or one
/// with both stubs while the meta project exists, needs none. A prompt that cannot be computed
/// is none.
pub(crate) fn init_prompt(dir: &Path) -> Option<&'static str> {
    let source = resolve_person_from_env().ok()?;
    match place_person(Some(dir), &source.path).ok()? {
        PersonPlace::ThisProject => None,
        PersonPlace::Missing(_) => Some(INIT_PROMPT),
        PersonPlace::Elsewhere(_) => {
            let stubbed = TARGETS
                .iter()
                .all(|t| matches!(preflight(&dir.join(t)), Ok(Plan::Keep)));
            (!stubbed).then_some(INIT_PROMPT)
        }
    }
}

/// The prompt of `rdy status` of `project` (agi4), or none: only a missing meta project gives
/// one.
pub(crate) fn status_prompt(project: &Path) -> Option<&'static str> {
    let source = resolve_person_from_env().ok()?;
    match place_person(Some(project), &source.path).ok()? {
        PersonPlace::Missing(_) => Some(STATUS_PROMPT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ain5_the_stub_is_three_lines() {
        assert_eq!(STUB.lines().count(), 3);
        assert!(STUB.ends_with('\n'));
        assert_eq!(
            STUB.lines().collect::<Vec<_>>(),
            [
                "This project keeps its agent instructions in rdy.",
                "At the start of every task, call rdy_instructions and load the rulesets your task needs.",
                "Required rules arrive without asking.",
            ]
        );
    }

    /// agi4: the meta project is the same under any spelling; an absent or unreadable path is
    /// not the meta project.
    #[test]
    fn agi4_is_meta_compares_canonical_identity() {
        let d = tempfile::tempdir().unwrap();
        let meta = d.path().join("m");
        std::fs::create_dir(&meta).unwrap();
        let alias = meta.join("..").join("m");
        assert!(is_meta(&alias, Some(&meta)));
        assert!(!is_meta(d.path(), Some(&meta)));
        assert!(!is_meta(&meta, Some(&d.path().join("absent"))));
        assert!(!is_meta(&meta, None));
    }

    /// agi4: the prompts and the occupied fix are the settled words.
    #[test]
    fn agi4_prompts_use_the_settled_words() {
        assert_eq!(
            INIT_PROMPT,
            "Run rdy setup to prepare your rules project and this project's agent instruction files."
        );
        assert_eq!(
            STATUS_PROMPT,
            "Your own rules project is missing. Run rdy setup."
        );
        assert_eq!(
            OCCUPIED_FIX,
            "Choose an unused absolute path with instructions.person, or move this path aside, then run rdy setup."
        );
    }
}
