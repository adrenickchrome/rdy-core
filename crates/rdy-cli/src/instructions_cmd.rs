//! `rdy instructions [--load <id>...]` (ain5, D-8X04F, D-YQPB6, D-A8N0E): the CLI fallback of
//! `rdy_instructions`. It gives the shared reply unchanged: the required instructions of the
//! project and the person layer in full, the rulesets it loads, and the menu. With no project
//! and no working-copy pointer, it and `rdy mcp` select the meta project, or, with no meta
//! project either, run with no project at all ([`select`]).
use crate::conn::{Env, VERSION, call, find_project, session_env};
use crate::output::text;
use rdy_daemon::ErrorReply;
use rdy_daemon::instructions::PersonReader;
use rdy_engine::config_levels::PROJECT_FILE;
use rdy_engine::instructions::{self, Caller, LoadRequest, PersonPlace, place_person};
use rdy_engine::instructions_config::{
    PathEnv, Platform, default_meta_path, resolve_person_from_env,
};
use rdy_engine::wc::state::{POINTER_FILE, pointer_project};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub(crate) struct InstructionsArgs {
    /// Rulesets to load, by their menu ids (project:<doc-id> or person:<doc-id>); a bare doc id
    /// loads when only one layer holds it. Give one or more ids, and repeat it as you like.
    /// Required rules arrive without it.
    #[arg(long, num_args = 1.., value_name = "ID")]
    load: Vec<String>,
}

/// A project directory found from the working directory: the directory whose marker was found
/// (`root`) and the project it is (`project`; a worktree's pointer names another).
pub(crate) struct Found {
    pub(crate) root: PathBuf,
    pub(crate) project: PathBuf,
}

/// The directory whose `.rdy` holds the default meta project (`<home>/.rdy/meta`), canonical.
fn meta_holder() -> Option<PathBuf> {
    let meta = default_meta_path(Platform::HOST, &PathEnv::from_process())?;
    fs::canonicalize(meta.parent()?.parent()?).ok()
}

/// The nearest project of `cwd`, as `rdy mcp`, `rdy instructions` and `rdy setup` find it with no
/// `--project`. A level counts when it has a working-copy pointer (its project), a `.rdy.yaml` or
/// a `.rdy` directory, so a damaged pointer and a partial project are found and refused, never
/// passed over. The one directory passed over is the home, whose `.rdy` holds the meta project:
/// with neither `.rdy.yaml` nor a store, it is no project. `None` when no level counts.
pub(crate) fn discover(cwd: &Path) -> Option<Found> {
    let holder = meta_holder();
    for dir in cwd.ancestors() {
        if fs::symlink_metadata(dir.join(POINTER_FILE)).is_ok_and(|m| m.is_file()) {
            return Some(Found {
                root: dir.to_path_buf(),
                project: pointer_project(dir).unwrap_or_else(|| find_project(dir)),
            });
        }
        let config = fs::symlink_metadata(dir.join(PROJECT_FILE)).is_ok();
        let rdy = fs::metadata(dir.join(".rdy")).is_ok_and(|m| m.is_dir());
        let store = fs::symlink_metadata(dir.join(".rdy").join("store")).is_ok();
        let home = holder
            .as_deref()
            .is_some_and(|h| fs::canonicalize(dir).is_ok_and(|d| d == h));
        if config || (rdy && (store || !home)) {
            return Some(Found {
                root: dir.to_path_buf(),
                project: dir.to_path_buf(),
            });
        }
    }
    None
}

/// What `rdy mcp` and `rdy instructions` run on with no `--project` (ain5).
pub(crate) enum Selected {
    /// A project: the nearest one, or the meta project when there is none.
    Project(PathBuf),
    /// No project and no meta project: the person layer is empty, with the creation hint.
    Projectless,
}

/// The project of `rdy mcp` and `rdy instructions` started in `cwd` with no `--project`: the
/// nearest project ([`discover`]); with none, the meta project the user config places
/// (D-YQPB6); with no meta project either, none. A config refusal is the command's refusal.
pub(crate) fn select(cwd: &Path) -> Result<Selected, ErrorReply> {
    if let Some(f) = discover(cwd) {
        return Ok(Selected::Project(f.project));
    }
    let source = resolve_person_from_env()?;
    Ok(match place_person(None, &source.path)? {
        PersonPlace::Elsewhere(meta) => Selected::Project(meta),
        PersonPlace::Missing(_) | PersonPlace::ThisProject => Selected::Projectless,
    })
}

/// The reader of the person layer for this process: through meta's daemon, or (with
/// `--no-daemon`) by opening meta's unused store here.
pub(crate) fn person_reader(no_daemon: bool) -> PersonReader {
    if no_daemon {
        PersonReader::direct(VERSION)
    } else {
        PersonReader::daemon(VERSION)
    }
}

/// `rdy instructions`: the ctl `instructions` op on the project, or with no project the person
/// layer alone. JSON is the shared reply unchanged.
pub(crate) fn run(
    env: &Env,
    a: InstructionsArgs,
    projectless: bool,
) -> Result<(Value, String), ErrorReply> {
    let load = (!a.load.is_empty()).then_some(a.load);
    let v = if projectless {
        outside(env, load)?
    } else {
        call(env, "instructions", json!({"load": load}))?
    };
    // the reply carries the user config's warnings; the others of this project go to stderr,
    // as every command prints them
    if !projectless {
        let carried: Vec<(&str, &str)> = v["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| (text(&w["code"]), text(&w["message"])))
            .collect();
        for w in rdy_engine::config_levels::config_warnings_for_env(&env.project) {
            if !carried.iter().any(|(c, m)| *c == w.code && *m == w.message) {
                eprintln!("{}", crate::config_warning_line(&w));
            }
        }
    }
    let t = reply_text(&v);
    Ok((v, t))
}

/// The reply outside every project, with no meta project to serve: the person layer through
/// this process's reader, asked as this process's actor and run.
fn outside(env: &Env, load: Option<Vec<String>>) -> Result<Value, ErrorReply> {
    let (actor, run) = session_env()?;
    let actor = match actor {
        Some(a) => a,
        None => rdy_engine::human_actor_for_env(&env.project)?,
    };
    let caller = Caller { actor, run };
    let load = LoadRequest::parse(&load.unwrap_or_default())?;
    let host = person_reader(env.no_daemon);
    let reply = instructions::deliver(None, &load, &caller, Some(&host))?;
    let bytes = reply.to_json()?;
    serde_json::from_slice(&bytes).map_err(|e| {
        ErrorReply::new(
            rdy_engine::E_INTERNAL,
            format!("an instructions reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// The reply as text: each instruction's whole body and its reasons, then the menu with each
/// ruleset's estimated tokens, then the warnings and the hint.
pub(crate) fn reply_text(v: &Value) -> String {
    let mut out = String::new();
    let rows = v["instructions"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        out.push_str("instructions: none\n");
    }
    for r in &rows {
        let locked = if r["locked"] == json!(true) {
            ", locked"
        } else {
            ""
        };
        out.push_str(&format!(
            "instruction {} ({}, ruleset {}{locked})\n{}\n",
            text(&r["id"]),
            text(&r["layer"]),
            text(&r["ruleset"]),
            text(&r["text"]).trim_end_matches('\n'),
        ));
        for reason in r["reason"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "reason {}: {}\n",
                text(&reason["id"]),
                text(&reason["statement"])
            ));
        }
        out.push('\n');
    }
    let menu = v["menu"].as_array().cloned().unwrap_or_default();
    if menu.is_empty() {
        out.push_str("menu: none\n");
    } else {
        out.push_str("menu (estimated tokens: UTF-8 bytes / 4, rounded up):\n");
    }
    for m in &menu {
        let required = if m["required"] == json!(true) {
            "required"
        } else {
            "optional"
        };
        out.push_str(&format!(
            "{} {} ({}, {required}, ~{} tokens)\n",
            text(&m["id"]),
            text(&m["name"]),
            text(&m["layer"]),
            m["tokens"].as_u64().unwrap_or(0),
        ));
        if !text(&m["when"]).is_empty() {
            out.push_str(&format!("  when: {}\n", text(&m["when"])));
        }
        let hints: Vec<&str> = m["hints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !hints.is_empty() {
            out.push_str(&format!("  hints: {}\n", hints.join(", ")));
        }
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "warning {}: {}; {}\n",
            text(&w["code"]),
            text(&w["message"]),
            text(&w["fix"])
        ));
    }
    if let Some(h) = v["hint"].as_str() {
        out.push_str(&format!("hint: {h}\n"));
    }
    out.trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        a: InstructionsArgs,
    }

    /// ain5: `--load` takes one or more ids and may be repeated; no other switch exists.
    #[test]
    fn ain5_load_takes_many_ids_and_repeats() {
        let p = |args: &[&str]| {
            let mut all = vec!["t"];
            all.extend_from_slice(args);
            T::try_parse_from(all).map(|t| t.a.load)
        };
        assert_eq!(p(&[]).unwrap(), Vec::<String>::new());
        assert_eq!(p(&["--load", "a", "b"]).unwrap(), ["a", "b"]);
        assert_eq!(
            p(&["--load", "a", "--load", "b", "c"]).unwrap(),
            ["a", "b", "c"]
        );
        assert!(p(&["--load"]).is_err());
        for flag in [
            "--scope",
            "--acts",
            "--path",
            "--max-tokens",
            "--page",
            "--force",
            "--required",
        ] {
            assert!(p(&[flag, "x"]).is_err(), "{flag}");
        }
    }

    /// ain5: the text gives whole bodies, reasons, the menu with tokens, warnings and the hint.
    #[test]
    fn ain5_text_prints_bodies_reasons_and_the_menu() {
        let v = json!({
            "instructions": [{"id": "ai-AAAAA", "ruleset": "project:d-AAAAA",
                "text": "One.\n\nTwo.", "layer": "project", "locked": true,
                "reason": [{"id": "D-AAAAA", "statement": "Because."}]}],
            "menu": [{"id": "project:d-AAAAA", "name": "Baseline", "when": "Always.",
                "layer": "project", "required": true, "tokens": 3, "hints": ["all", "act:write"]}],
            "warnings": [{"code": "W_CONFIG_UNKNOWN", "message": "m", "fix": "f"}],
            "hint": "Create it."
        });
        assert_eq!(
            reply_text(&v),
            "instruction ai-AAAAA (project, ruleset project:d-AAAAA, locked)\nOne.\n\nTwo.\nreason D-AAAAA: Because.\n\nmenu (estimated tokens: UTF-8 bytes / 4, rounded up):\nproject:d-AAAAA Baseline (project, required, ~3 tokens)\n  when: Always.\n  hints: all, act:write\nwarning W_CONFIG_UNKNOWN: m; f\nhint: Create it."
        );
        assert_eq!(
            reply_text(&json!({"instructions": [], "menu": [], "hint": "h"})),
            "instructions: none\nmenu: none\nhint: h"
        );
    }
}
