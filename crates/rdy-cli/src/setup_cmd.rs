//! `rdy setup` (ain5, D-8X04F; agi4, D-PHH2Q; m4a3, D-DC5D1, D-KG0V6, D-EKGN9): the person's
//! empty rules project, then the stub, then the hooks asked for. It creates the meta project at
//! the configured place when nothing stands there, and writes three lines into `CLAUDE.md` and
//! `AGENTS.md` of the chosen project root. Outside a project, or inside the meta project, it
//! writes no stub. The stub goes first, before the person's own text, which stays byte for byte;
//! a file that already holds the stub is left alone, and a file that holds the 0.43.0 to 0.45.0
//! stub gets the new one in its place. A target that is not a regular file is refused before
//! either file is written, and a link is never followed. The meta project never gets rules, a
//! copy of the guide, a policy or a stub. A failed meta step is a warning: the project's stubs
//! still go in.
//!
//! After those steps, and only when asked, it installs the optional warning hooks through the
//! local hook host: `--code-hook <repo>` the Git `commit-msg` warning of a linked code repo, and
//! `--edit-hook claude-code` Claude Code's `PostToolUse` warning in the project root. Their
//! results go in a `hooks` member, which bare setup does not have. A hook with no project to go
//! in (outside a project, in the meta project, or a repo that is not linked) is skipped with a
//! warning; nothing is written elsewhere. No harness detection, MCP registration, skill,
//! capture or wrap-up guidance, Stop hook, download or rule import is part of it.
use crate::conn::{is_project, require_project};
use crate::instructions_cmd::discover;
use clap::Args;
use rdy_daemon::ErrorReply;
use rdy_engine::hooks::{
    HOOK_DEADLINE, HookHost, HookInstall, HookKind, HookRequest, HookWarning, RunContext,
    Unavailable,
};
use rdy_engine::instructions::{PersonPlace, place_person};
use rdy_engine::instructions_config::{PersonPath, PersonSource, resolve_person_from_env};
use rdy_engine::{E_BAD_INPUT, E_IO, E_LIMIT, E_NO_PROJECT, Rdy, RdyError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The optional hooks `rdy setup` installs after its own steps; none unless asked.
#[derive(Args, Default)]
pub(crate) struct SetupArgs {
    /// Also install the Git commit-msg warning in this linked code repo's checkout.
    #[arg(long, value_name = "REPO")]
    code_hook: Option<String>,
    /// Also install the edit warning for this harness in the project root.
    #[arg(long, value_name = "HARNESS", value_parser = ["claude-code"])]
    edit_hook: Option<String>,
}

/// The stub, with its final newline.
pub(crate) const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_session, then rdy_instructions, and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

/// The stub of 0.43.0 to 0.45.0. A file that holds it gets the current stub in its place.
const OLD_STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

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

/// A path as a message shows it: no `\\?\` prefix on a plain Windows drive path.
fn shown(path: &Path) -> String {
    rdy_engine::display_path(path)
}

/// What one target needs.
enum Plan {
    /// No file: it is created with the stub.
    Create,
    /// A regular file without the stub: the stub goes before its bytes, and the file keeps its
    /// permissions.
    Prepend(Vec<u8>, std::fs::Permissions),
    /// A regular file that holds the old stub: the new stub takes its place, every other byte
    /// stays, and the file keeps its permissions.
    Replace(Vec<u8>, std::fs::Permissions),
    /// A regular file that holds the stub: left alone.
    Keep,
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", shown(path)),
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
            format!("{} is not a regular instruction file", shown(path)),
            NOT_REGULAR_FIX,
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| io(path, e))?;
    let has = bytes.windows(STUB.len()).any(|w| w == STUB.as_bytes());
    if has {
        return Ok(Plan::Keep);
    }
    let old = OLD_STUB.as_bytes();
    if let Some(at) = bytes.windows(old.len()).position(|w| w == old) {
        let mut out = bytes[..at].to_vec();
        out.extend_from_slice(STUB.as_bytes());
        out.extend_from_slice(&bytes[at + old.len()..]);
        return Ok(Plan::Replace(out, meta.permissions()));
    }
    Ok(Plan::Prepend(bytes, meta.permissions()))
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
                text.push(format!("{} already has the stub", shown(path)));
                continue;
            }
            Plan::Create => (STUB.as_bytes().to_vec(), None),
            Plan::Replace(bytes, perms) => (bytes, Some(perms)),
            Plan::Prepend(old, perms) => ([STUB.as_bytes(), &old].concat(), Some(perms)),
        };
        let wrote = rdy_engine::write_durable(path, &bytes)
            .and_then(|()| perms.map_or(Ok(()), |p| std::fs::set_permissions(path, p)));
        if let Err(e) = wrote {
            let done: Vec<String> = written
                .iter()
                .map(|p| format!("; {} was written", shown(p)))
                .collect();
            return Err(RdyError::new(
                E_IO,
                format!("{}: {e}{}", shown(path), done.concat()),
                "fix the cause, then run rdy setup again; it leaves a file that has the stub alone",
            )
            .into());
        }
        written.push(path);
        files.push(json!({"path": path.to_string_lossy(), "result": "written"}));
        text.push(format!("wrote the stub to {}", shown(path)));
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
            shown(path),
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
        format!("Your rules project already exists at {}.", shown(path)),
    )
}

fn occupied(path: &Path) -> Meta {
    Meta::warn(
        Some(path),
        format!(
            "Cannot create your rules project at {}: the path is not an rdy project.",
            shown(path)
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
                format!("{}: {e}", shown(path)),
                "check the path and its directory, then run rdy setup again",
            );
            return cannot_create(path, &e);
        }
    }
    match Rdy::init(path, "rdy-standard").and_then(Rdy::close) {
        Ok(()) => Meta::done(
            path,
            "created",
            format!("Created your rules project at {}.", shown(path)),
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
pub(crate) fn setup(
    chosen: Option<Chosen>,
    args: &SetupArgs,
) -> Result<(Value, String), ErrorReply> {
    let (meta, warnings, meta_path) = meta_step();
    // the project a hook goes in: only one whose stubs were processed
    let mut target: Option<&Path> = None;
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
            target = Some(&c.project);
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
    let mut v = json!({"files": files, "meta": meta.json(), "stub": stub, "warnings": warnings});
    let hooks = hooks(target, args);
    if !hooks.is_empty() {
        for h in &hooks {
            let t = crate::hook_cmd::install_text(h);
            if !t.is_empty() {
                lines.push(t);
            }
        }
        v["hooks"] = Value::Array(hooks);
    }
    Ok((v, lines.join("\n")))
}

/// The hooks asked for, each installed in `project` through the local hook host, the code hook
/// first: one install result per hook. With no project, or a target the host refuses, the hook
/// is skipped with a `W_HOOK_UNAVAILABLE` warning and no file.
fn hooks(project: Option<&Path>, args: &SetupArgs) -> Vec<Value> {
    let mut asked: Vec<(HookKind, Value)> = vec![];
    if let Some(repo) = &args.code_hook {
        asked.push((
            HookKind::Code,
            json!({"op": "install", "kind": "code", "repo": repo}),
        ));
    }
    if let Some(harness) = &args.edit_hook {
        asked.push((
            HookKind::Edit,
            json!({"op": "install", "kind": "edit", "harness": harness}),
        ));
    }
    asked
        .into_iter()
        .map(|(kind, req)| {
            let installed = match project {
                None => Err(None),
                Some(p) => install(p, &req).map_err(Some),
            };
            let result = installed.unwrap_or_else(|e| {
                let reason = match e.as_ref().map(|e| e.code) {
                    None => Unavailable::Project,
                    // the one target a bare form can miss: a repo that is not linked
                    Some(E_BAD_INPUT) => Unavailable::Checkout,
                    Some(E_LIMIT) => Unavailable::InputLimit,
                    Some(E_IO) => Unavailable::Io,
                    Some(_) => Unavailable::Project,
                };
                HookInstall {
                    kind,
                    files: vec![],
                    warnings: vec![HookWarning::unavailable(reason, e.as_ref())],
                }
            });
            serde_json::to_value(result).unwrap_or(Value::Null)
        })
        .collect()
}

/// One install request through the local hook host of `project`, run by this binary, under the
/// host deadline.
fn install(project: &Path, req: &Value) -> Result<HookInstall, RdyError> {
    let HookRequest::Install(target) = HookRequest::from_value(req)? else {
        unreachable!("setup asks only for installs");
    };
    let host = rdy_daemon::hooks::LocalHooks::for_process(project)
        .map_err(|e| RdyError::new(E_IO, e.to_string(), "run rdy setup again"))?;
    let ctx = RunContext::new().with_deadline(std::time::Instant::now() + HOOK_DEADLINE);
    host.install(&target, &ctx)
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
                "At the start of every task, call rdy_session, then rdy_instructions, and load the rulesets your task needs.",
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
