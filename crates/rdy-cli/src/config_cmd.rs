//! `rdy config [--show-origin]`: the settings in effect, one row per key of `KEY_LEVELS` in table
//! order (an unset key shows `unset`), each with the levels that may set it; `code_paths` shows one row per repo, each with the level it comes
//! from (F1). Runs in this process: it reads files and needs no daemon.
use crate::ErrorReply;
use rdy_engine::config_levels::{
    CODE_PATHS_KEY, KEY_LEVELS, Origin, inspect, pack_warnings, user_file_from_env,
};
use rdy_engine::hub_config::{HUB_INSTALLS_KEY, REMOTES_KEY};
use serde_json::{Value, json};
use std::path::Path;

/// One listed row: the origin that supplies it (`None` for an unset key) and the levels the
/// key's table entry allows.
struct Row<'a> {
    key: &'a str,
    origin: Option<&'a Origin>,
    levels: Vec<String>,
}

/// The rows of the table, in table order. A key with a value has its origin row; a group
/// (`code_paths`, `remotes`, `hub.installs`) has one row per entry; a key or group with nothing
/// is one unset row.
fn rows<'a>(origins: &'a [Origin]) -> Vec<Row<'a>> {
    let mut out = Vec::with_capacity(KEY_LEVELS.len());
    for rule in KEY_LEVELS {
        let levels: Vec<String> = rule
            .levels
            .iter()
            .filter_map(|l| serde_json::to_value(l).ok())
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let is_group = [CODE_PATHS_KEY, REMOTES_KEY, HUB_INSTALLS_KEY].contains(&rule.key);
        let before = out.len();
        for o in origins {
            let hit = if is_group {
                o.key
                    .strip_prefix(rule.key)
                    .is_some_and(|rest| rest.starts_with('.'))
            } else {
                o.key == rule.key
            };
            if hit {
                out.push(Row {
                    key: &o.key,
                    origin: Some(o),
                    levels: levels.clone(),
                });
            }
        }
        if out.len() == before {
            out.push(Row {
                key: rule.key,
                origin: None,
                levels,
            });
        }
    }
    out
}

pub fn run(project: &Path, show_origin: bool) -> Result<(Value, String), ErrorReply> {
    // an actor outside the form is shown as written; resolving the person refuses it
    let eff = inspect(project, user_file_from_env().as_deref())?;
    let rows = rows(&eff.origins);
    let mut lines = Vec::with_capacity(rows.len());
    for r in &rows {
        let value = r
            .origin
            .map_or_else(|| "unset".to_string(), Origin::value_line);
        let mut line = format!("{} = {value}", r.key);
        if show_origin {
            match r.origin {
                None => line.push_str("  # unset"),
                Some(o) => match &o.file {
                    Some(f) => line.push_str(&format!("  # {}", f.display())),
                    None => line.push_str("  # default"),
                },
            }
        }
        line.push_str(&format!("  # set at: {}", r.levels.join(", ")));
        lines.push(line);
    }
    if show_origin {
        // the config warnings, then the pack's (O15): the warnings every other command prints
        // on stderr
        for w in eff.warnings.iter().chain(&pack_warnings(project)) {
            lines.push(format!("warning {}: {}; {}", w.code, w.message, w.fix));
        }
    }
    let json_rows: Vec<Value> = rows
        .iter()
        .map(|r| match r.origin {
            Some(o) => json!({"key": r.key, "value": o.value, "level": o.level,
                "file": o.file, "levels": r.levels}),
            None => json!({"key": r.key, "value": null, "level": null, "file": null,
                "levels": r.levels}),
        })
        .collect();
    Ok((Value::Array(json_rows), lines.join("\n")))
}

#[derive(clap::Args)]
pub(crate) struct ConfigArgs {
    /// Also name the file each value comes from, or `default`.
    #[arg(long)]
    pub(crate) show_origin: bool,
}
