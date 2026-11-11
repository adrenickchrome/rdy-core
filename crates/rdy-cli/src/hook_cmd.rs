//! `rdy hook` (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1; first edit adapter
//! D-KG0V6): the command-line twin of `rdy_hook`, and the two host entry points the installed
//! hooks call.
//!
//! - `rdy hook check --kind <code|edit> --input <json-file>` runs the shared check on the form's
//!   other fields, read from the file.
//! - `rdy hook install|remove --kind code --repo <name>` and `--kind edit --harness claude-code
//!   [--worktree <name>]` install or remove a hook through the project's local hook host.
//! - `rdy hook run-code --repo <name> --message-file <path>` is what Git's `commit-msg` launcher
//!   runs, and `rdy hook run-edit --harness claude-code` what Claude Code's `PostToolUse` entry
//!   runs with one event on stdin.
//!
//! The public commands keep the ordinary CLI contract: a usage error is the parser's exit 2 and
//! a refused form exits 1. The host entry points never fail the action they look at: every
//! operational failure is a `W_HOOK_UNAVAILABLE` warning, and they exit 0. Each host run is
//! bounded by [`HOOK_DEADLINE`]: a worker process (this binary again) does the work under a run
//! context with a slightly earlier deadline, so it stops and reaps its own Git children, and the
//! entry point kills and reaps the worker through its own process handle if it is still running
//! at the deadline. A worker that dies closes its daemon connection, which cancels the daemon's
//! run. The worker reads the caller's staged diff in this process, with the index Git names;
//! the daemon never infers it.
use crate::conn::{Conn, Env, call, find_project};
use clap::{Args, Subcommand};
use rdy_daemon::ErrorReply;
use rdy_engine::hooks::{
    HOOK_DEADLINE, HookCheck, HookKind, HookWarning, MAX_HOOK_INPUT, MAX_HOOK_MESSAGE, RunContext,
    Unavailable, collect_staged, form_error, limit,
};
use rdy_engine::{E_IO, RdyError};
use serde_json::{Map, Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

#[derive(Subcommand)]
pub(crate) enum HookCmd {
    /// Run the shared hook check: rdy hook check --kind <code|edit> --input <json-file>. The file holds the check's other fields: repo, checkout, head, paths and message for code; paths for edit. It warns and changes nothing. The chat path is rdy_hook.
    Check(CheckArgs),
    /// Install a hook: the Git commit-msg warning of a linked code repo (--kind code --repo <name>), or Claude Code's PostToolUse warning for Edit and Write (--kind edit --harness claude-code [--worktree <name>]). A person's own file is never replaced. The chat path is rdy_hook.
    Install(TargetArgs),
    /// Remove a hook that rdy installed: only its exact bytes or settings entry go. The chat path is rdy_hook.
    Remove(TargetArgs),
    /// The code hook's entry point, run by Git's commit-msg launcher: warns when the staged change touches code anchored to a decision and the message has no Implements trailer. It always exits 0.
    RunCode(RunCodeArgs),
    /// The edit hook's entry point, run by Claude Code after an Edit or Write with the event on standard input: warns when the edit changed rdy working-copy files. It always exits 0.
    RunEdit(RunEditArgs),
}

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// The check: code (a commit) or edit (an agent's edit).
    #[arg(long, value_parser = ["code", "edit"])]
    kind: String,
    /// A JSON file with the check's other fields.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
pub(crate) struct TargetArgs {
    /// The hook: code (Git commit-msg) or edit (Claude Code PostToolUse).
    #[arg(long, value_parser = ["code", "edit"])]
    kind: String,
    /// The linked code repo, as code_repos names it (--kind code).
    #[arg(long)]
    repo: Option<String>,
    /// The harness of an edit hook (--kind edit).
    #[arg(long, value_parser = ["claude-code"])]
    harness: Option<String>,
    /// A registered worktree to install the edit hook in (default: the project root).
    #[arg(long)]
    worktree: Option<String>,
}

#[derive(Args)]
pub(crate) struct RunCodeArgs {
    /// The linked code repo, as code_repos names it.
    #[arg(long)]
    repo: String,
    /// The commit message file Git passes to its commit-msg hook.
    #[arg(long)]
    message_file: PathBuf,
}

#[derive(Args)]
pub(crate) struct RunEditArgs {
    /// The harness that sends the event.
    #[arg(long, value_parser = ["claude-code"])]
    harness: String,
}

// ---------------------------------------------------------------------------------------------
// The public commands
// ---------------------------------------------------------------------------------------------

/// `rdy hook check|install|remove`: one `hook` op, through the daemon or in this process.
pub(crate) fn run(env: &Env, cmd: HookCmd) -> Result<(Value, String), ErrorReply> {
    match cmd {
        HookCmd::Check(a) => {
            let req = check_request(&a)?;
            let v = call(env, "hook", req)?;
            let text = check_text(&v);
            Ok((v, text))
        }
        HookCmd::Install(a) => target(env, "install", &a),
        HookCmd::Remove(a) => target(env, "remove", &a),
        HookCmd::RunCode(_) | HookCmd::RunEdit(_) => unreachable!("handled in main"),
    }
}

/// Whether `cmd` is a host entry point, which main runs before any other step that can fail.
pub(crate) fn is_host(cmd: &HookCmd) -> bool {
    matches!(cmd, HookCmd::RunCode(_) | HookCmd::RunEdit(_))
}

/// At most `max` bytes of the file at `path`; more is `E_LIMIT` in `unit`, refused before it is
/// read in full.
fn read_bounded(path: &Path, max: usize, unit: &str) -> Result<Vec<u8>, RdyError> {
    let io = |e: std::io::Error| {
        RdyError::new(
            E_IO,
            format!("{}: {e}", rdy_engine::display_path(path)),
            "check the file, then run the hook again",
        )
    };
    let file = std::fs::File::open(path).map_err(io)?;
    bounded(file, max, unit)
}

/// At most `max` bytes of `r`; more is `E_LIMIT` in `unit`, a read failure `E_IO`.
fn bounded(r: impl Read, max: usize, unit: &str) -> Result<Vec<u8>, RdyError> {
    let mut buf = Vec::new();
    r.take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| RdyError::new(E_IO, e.to_string(), "run the hook again"))?;
    if buf.len() > max {
        return Err(limit(max, unit));
    }
    Ok(buf)
}

/// The `check` request: the input file's fields (a JSON object of at most [`MAX_HOOK_INPUT`]
/// bytes, naming neither `op` nor `kind`) with the command's `op` and `kind`.
fn check_request(a: &CheckArgs) -> Result<Value, ErrorReply> {
    let bytes = read_bounded(&a.input, MAX_HOOK_INPUT, "bytes")?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| form_error(format!("the input is not JSON ({e})")))?;
    let Value::Object(mut fields) = v else {
        return Err(form_error("the input is not a JSON object").into());
    };
    for key in ["op", "kind"] {
        if fields.contains_key(key) {
            return Err(form_error(format!(
                "remove {key} from the input: the command line gives it"
            ))
            .into());
        }
    }
    fields.insert("op".into(), json!("check"));
    fields.insert("kind".into(), json!(a.kind));
    Ok(Value::Object(fields))
}

/// The `install` or `remove` request of `a`: only the options given.
fn target_request(op: &str, a: &TargetArgs) -> Value {
    let mut m = Map::new();
    m.insert("op".into(), json!(op));
    m.insert("kind".into(), json!(a.kind));
    for (key, value) in [
        ("repo", &a.repo),
        ("harness", &a.harness),
        ("worktree", &a.worktree),
    ] {
        if let Some(v) = value {
            m.insert(key.into(), json!(v));
        }
    }
    Value::Object(m)
}

fn target(env: &Env, op: &str, a: &TargetArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "hook", target_request(op, a))?;
    let text = install_text(&v);
    Ok((v, text))
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// The lines of one hook warning: its code and message, its fix, the paths it names, how many
/// more, and the underlying error.
pub(crate) fn warning_lines(w: &Value) -> Vec<String> {
    let s = |k: &str| w[k].as_str().unwrap_or_default();
    let mut out = vec![
        format!("{}: {}", s("code"), s("message")),
        format!("fix: {}", s("fix")),
    ];
    for p in w["paths"].as_array().into_iter().flatten() {
        out.push(format!("  {}", p.as_str().unwrap_or_default()));
    }
    if let Some(n) = w["omitted"].as_u64() {
        out.push(format!("  and {n} more"));
    }
    if let Some(d) = w.get("diagnostic") {
        out.push(format!(
            "  ({}: {})",
            d["code"].as_str().unwrap_or_default(),
            d["message"].as_str().unwrap_or_default()
        ));
    }
    out
}

/// Every warning of a result, as lines.
fn warnings_text(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(warning_lines)
        .collect()
}

/// A check's text: its warnings, or that it found nothing. An incomplete check always carries
/// its unavailable warning, so it never reads as clean.
fn check_text(v: &Value) -> String {
    let lines = warnings_text(v);
    if lines.is_empty() && v["checked"] == true {
        return format!(
            "checked {}: no warning",
            v["kind"].as_str().unwrap_or_default()
        );
    }
    lines.join("\n")
}

/// An install's or remove's text: each file with its result, then the warnings.
pub(crate) fn install_text(v: &Value) -> String {
    let kind = v["kind"].as_str().unwrap_or_default();
    let mut lines: Vec<String> = v["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            format!(
                "{kind} hook {}: {}",
                f["result"].as_str().unwrap_or_default(),
                f["path"].as_str().unwrap_or_default()
            )
        })
        .collect();
    lines.extend(warnings_text(v));
    lines.join("\n")
}

// ---------------------------------------------------------------------------------------------
// The host entry points
// ---------------------------------------------------------------------------------------------

/// Set in the worker's environment: this process does the run and prints its result as JSON.
const WORKER_ENV: &str = "RDY_HOOK_WORKER";

/// How long before the host deadline the worker's own run stops, so it can reap its Git
/// children and answer before the entry point kills it.
const CLEANUP_MARGIN: Duration = Duration::from_millis(250);

/// The most bytes of a worker's answer the entry point reads.
const MAX_WORKER_REPLY: usize = MAX_HOOK_INPUT;

fn kind_of(cmd: &HookCmd) -> HookKind {
    match cmd {
        HookCmd::RunCode(_) => HookKind::Code,
        _ => HookKind::Edit,
    }
}

/// The unavailable check of `kind` for `reason`, as JSON.
fn unavailable(kind: HookKind, reason: Unavailable, cause: Option<&RdyError>) -> Value {
    to_json(&HookCheck {
        kind,
        checked: false,
        warnings: vec![HookWarning::unavailable(reason, cause)],
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// A daemon or connection refusal as the engine error a host run's warning carries: the
/// engine's own spelling of its code when it has one.
fn engine_error(e: ErrorReply) -> RdyError {
    let daemon = [
        rdy_daemon::E_DAEMON_START,
        rdy_daemon::E_DAEMON_VERSION,
        rdy_daemon::E_DAEMON_GONE,
    ];
    match rdy_engine::core_error_code(&e.code).or_else(|| daemon.into_iter().find(|c| *c == e.code))
    {
        Some(code) => RdyError::new(code, e.message, e.fix),
        None => RdyError::new(E_IO, format!("{}: {}", e.code, e.message), e.fix),
    }
}

/// A host entry point (`rdy hook run-code`, `rdy hook run-edit`): the run under the host
/// deadline, then its output. It exits 0 whatever happened.
pub(crate) fn host(cmd: &HookCmd, json: bool, project: Option<&Path>, no_daemon: bool) -> ExitCode {
    let kind = kind_of(cmd);
    if std::env::var_os(WORKER_ENV).is_some() {
        let ctx = RunContext::new()
            .with_deadline(Instant::now() + HOOK_DEADLINE.saturating_sub(CLEANUP_MARGIN));
        let v = work(cmd, kind, project, no_daemon, &ctx);
        let _ = writeln!(std::io::stdout(), "{v}");
        return ExitCode::SUCCESS;
    }
    let v = supervise(kind);
    emit(kind, &v, json);
    ExitCode::SUCCESS
}

/// Runs this binary again as the worker of this host run, with the same arguments, and waits
/// for its answer until [`HOOK_DEADLINE`]. A worker still running then is killed and reaped
/// through its own handle, and the answer is the `deadline` warning.
fn supervise(kind: HookKind) -> Value {
    let deadline = Instant::now() + HOOK_DEADLINE;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            let e = RdyError::new(E_IO, e.to_string(), "run the hook again");
            return unavailable(kind, Unavailable::Io, Some(&e));
        }
    };
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .env(WORKER_ENV, "1")
        // the edit hook's event is on stdin: the worker reads it, under the deadline
        .stdin(match kind {
            HookKind::Edit => Stdio::inherit(),
            HookKind::Code => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    rdy_engine::no_console_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let e = RdyError::new(
                E_IO,
                format!("starting the hook worker: {e}"),
                "run the hook again",
            );
            return unavailable(kind, Unavailable::Io, Some(&e));
        }
    };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let e = RdyError::new(
            E_IO,
            "the hook worker has no output pipe",
            "run the hook again",
        );
        return unavailable(kind, Unavailable::Io, Some(&e));
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // left behind on a kill: the dead worker closes the pipe and ends the read
    std::thread::spawn(move || {
        let _ = tx.send(bounded(out, MAX_WORKER_REPLY, "bytes"));
    });
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left) {
        Ok(read) => {
            let _ = child.wait();
            match read {
                Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .filter(|v| v["kind"].is_string() && v["warnings"].is_array())
                    .unwrap_or_else(|| {
                        let e = RdyError::new(
                            E_IO,
                            "the hook worker gave no result",
                            "run the hook again",
                        );
                        unavailable(kind, Unavailable::Io, Some(&e))
                    }),
                Err(e) => unavailable(kind, Unavailable::Io, Some(&e)),
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            unavailable(kind, Unavailable::Deadline, None)
        }
    }
}

/// Prints a host run's result. The code hook: the result as JSON with `--json`, else its
/// warnings on stderr (Git shows them; a clean check prints nothing). The edit hook: Claude
/// Code's `hookSpecificOutput` with the warnings as additional context, and nothing for a clean
/// check; no decision, no blocking field and no change to the tool's output.
fn emit(kind: HookKind, v: &Value, json: bool) {
    let lines = warnings_text(v);
    match kind {
        HookKind::Code if json => println!("{v}"),
        HookKind::Code => {
            for line in lines {
                eprintln!("{line}");
            }
        }
        HookKind::Edit => {
            if !lines.is_empty() {
                let out = json!({"hookSpecificOutput": {
                    "hookEventName": rdy_daemon::hooks::EDIT_EVENT,
                    "additionalContext": lines.join("\n"),
                }});
                println!("{out}");
            }
        }
    }
}

/// The worker's run: the check's result as JSON, or the unavailable warning that says why.
fn work(
    cmd: &HookCmd,
    kind: HookKind,
    project: Option<&Path>,
    no_daemon: bool,
    ctx: &RunContext,
) -> Value {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            let e = RdyError::new(E_IO, e.to_string(), "run from an existing directory");
            return unavailable(kind, Unavailable::Project, Some(&e));
        }
    };
    let env = Env {
        project: project
            .map(Path::to_path_buf)
            .unwrap_or_else(|| find_project(&cwd)),
        cwd,
        no_daemon: no_daemon || std::env::var("RDY_NO_DAEMON").is_ok_and(|v| v == "1"),
        place: None,
    };
    let req = match cmd {
        HookCmd::RunCode(a) => code_request(&env, a, ctx),
        HookCmd::RunEdit(_) => edit_request(),
        _ => unreachable!("only host entry points run as workers"),
    };
    match req {
        Ok(req) => hook_call(&env, kind, req, ctx),
        Err(Stopped::Check(check)) => to_json(&check),
        Err(Stopped::Refused(e)) => rdy_daemon::hooks::warn_only(kind, Err(e)),
    }
}

/// Why a host run has no request to send.
enum Stopped {
    /// The collector's own unavailable result.
    Check(HookCheck),
    /// A refusal, which becomes its unavailable warning.
    Refused(RdyError),
}

/// The code check of the commit Git is making: the message, then the caller's checkout, HEAD
/// and staged paths, collected here with the index Git names.
fn code_request(env: &Env, a: &RunCodeArgs, ctx: &RunContext) -> Result<Value, Stopped> {
    let message = read_bounded(&a.message_file, MAX_HOOK_MESSAGE, "message bytes")
        .map_err(Stopped::Refused)?;
    let index = std::env::var_os("GIT_INDEX_FILE");
    let staged = collect_staged(&env.cwd, index.as_deref(), ctx).map_err(Stopped::Check)?;
    Ok(json!({
        "op": "check",
        "kind": "code",
        "repo": a.repo,
        "checkout": staged.checkout,
        "head": staged.head,
        "paths": staged.paths,
        "message": String::from_utf8_lossy(&message),
    }))
}

/// The edit check of one Claude Code event on stdin: a `PostToolUse` event of `Edit` or `Write`,
/// whose file path alone is sent. Its other fields (the transcript, the content, the tool's
/// output) are never read further or sent.
fn edit_request() -> Result<Value, Stopped> {
    let bytes =
        bounded(std::io::stdin().lock(), MAX_HOOK_INPUT, "bytes").map_err(Stopped::Refused)?;
    let invalid = |why: &str| Stopped::Refused(form_error(format!("the event {why}")));
    let event: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("is not JSON"))?;
    if event["hook_event_name"] != rdy_daemon::hooks::EDIT_EVENT {
        return Err(invalid("is no PostToolUse event"));
    }
    if !matches!(event["tool_name"].as_str(), Some("Edit" | "Write")) {
        return Err(invalid("is no Edit or Write result"));
    }
    let Some(path) = event["tool_input"]["file_path"].as_str() else {
        return Err(invalid("names no file path"));
    };
    Ok(json!({"op": "check", "kind": "edit", "paths": [path]}))
}

/// One `hook` op under `ctx`: through the daemon, or in this process with the local hook host.
/// Every refusal is its unavailable warning, and a passed deadline the `deadline` one.
fn hook_call(env: &Env, kind: HookKind, req: Value, ctx: &RunContext) -> Value {
    if ctx.stopped().is_some() {
        return unavailable(kind, Unavailable::Deadline, None);
    }
    let mut conn = match Conn::open(env) {
        Ok(c) => c,
        Err(e) => return rdy_daemon::hooks::warn_only(kind, Err(engine_error(e))),
    };
    let out = conn.op_within("hook", req, ctx);
    // a connection the deadline left waiting closes here, which cancels the daemon's run
    conn.close();
    match out {
        Ok(Some(v)) => v,
        Ok(None) => unavailable(kind, Unavailable::Deadline, None),
        Err(e) => rdy_daemon::hooks::warn_only(kind, Err(engine_error(e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// m4a3: a warning's lines name its code, message, fix, paths, the omitted count and the
    /// underlying error; a clean check says so, and an incomplete one never reads as clean.
    #[test]
    fn m4a3_hook_text_never_shows_an_incomplete_check_as_clean() {
        let w = json!({"code": "W_HOOK_NO_CITATION", "message": "m.", "fix": "f.",
            "paths": ["a.rs"], "omitted": 2,
            "diagnostic": {"code": "E_IO", "message": "x"}});
        assert_eq!(
            warning_lines(&w),
            [
                "W_HOOK_NO_CITATION: m.",
                "fix: f.",
                "  a.rs",
                "  and 2 more",
                "  (E_IO: x)"
            ]
        );
        assert_eq!(
            check_text(&json!({"kind": "code", "checked": true, "warnings": []})),
            "checked code: no warning"
        );
        let incomplete = json!({"kind": "edit", "checked": false, "warnings": [
            {"code": "W_HOOK_UNAVAILABLE", "message": "The rdy hook could not check this action: deadline.", "fix": "f."}]});
        assert!(check_text(&incomplete).starts_with("W_HOOK_UNAVAILABLE"));
        assert_eq!(
            install_text(
                &json!({"kind": "code", "files": [{"path": "/x/commit-msg", "result": "kept"}], "warnings": []})
            ),
            "code hook kept: /x/commit-msg"
        );
    }

    /// m4a3: install and remove send only the options given; the shared form refuses the rest.
    #[test]
    fn m4a3_target_requests_send_only_given_options() {
        let a = TargetArgs {
            kind: "edit".into(),
            repo: None,
            harness: Some("claude-code".into()),
            worktree: Some("wt".into()),
        };
        assert_eq!(
            target_request("install", &a),
            json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "wt"})
        );
        let a = TargetArgs {
            kind: "code".into(),
            repo: Some("app".into()),
            harness: None,
            worktree: None,
        };
        assert_eq!(
            target_request("remove", &a),
            json!({"op": "remove", "kind": "code", "repo": "app"})
        );
    }

    /// m4a3: a bounded read refuses one byte past its bound before keeping it all.
    #[test]
    fn m4a3_bounded_reads_refuse_past_the_bound() {
        assert_eq!(bounded(&b"abc"[..], 3, "bytes").unwrap(), b"abc");
        let e = bounded(&b"abcd"[..], 3, "bytes").unwrap_err();
        assert_eq!(e.code, rdy_engine::E_LIMIT);
        assert_eq!(e.message, "The hook input exceeds 3 bytes.");
    }
}
