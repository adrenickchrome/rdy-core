//! `rdy config [--show-origin]`: the settings in effect, one row per key that has a value, in
//! the order of `KEY_LEVELS`; `code_paths` shows one row per repo, each with the level it comes
//! from (F1). Runs in this process: it reads files and needs no daemon.
use crate::ErrorReply;
use rdy_engine::config_levels::{inspect, pack_warnings, user_file_from_env};
use serde_json::{Value, json};
use std::path::Path;

pub fn run(project: &Path, show_origin: bool) -> Result<(Value, String), ErrorReply> {
    // an actor outside the form is shown as written; resolving the person refuses it
    let eff = inspect(project, user_file_from_env().as_deref())?;
    let mut lines = Vec::with_capacity(eff.origins.len());
    for o in &eff.origins {
        let mut line = format!("{} = {}", o.key, o.value_line());
        if show_origin {
            match &o.file {
                Some(f) => line.push_str(&format!("  # {}", f.display())),
                None => line.push_str("  # default"),
            }
        }
        lines.push(line);
    }
    if show_origin {
        // the config warnings, then the pack's (O15): the warnings every other command prints
        // on stderr
        for w in eff.warnings.iter().chain(&pack_warnings(project)) {
            lines.push(format!("warning {}: {}; {}", w.code, w.message, w.fix));
        }
    }
    let rows: Vec<Value> = eff
        .origins
        .iter()
        .map(|o| json!({"key": o.key, "value": o.value, "level": o.level, "file": o.file}))
        .collect();
    Ok((Value::Array(rows), lines.join("\n")))
}

#[derive(clap::Args)]
pub(crate) struct ConfigArgs {
    /// Also name the file each value comes from, or `default`.
    #[arg(long)]
    pub(crate) show_origin: bool,
}
