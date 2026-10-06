//! cview1: `rdy view *` and `rdy undo`. The commands build one `ViewRequest` JSON for the ctl
//! `view` op; the renderers turn the op's reply into text. `--json` prints the reply as it is.
use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum ViewCmd {
    /// Create a view (it floats on canon unless pinned).
    New {
        name: String,
        /// The landing to start from: canon (default), landing:<id> or tag:<version>.
        #[arg(long)]
        base: Option<String>,
        /// Keep the base: the view is not rebased when something lands.
        #[arg(long)]
        pin: bool,
    },
    /// Copy a view's changes into a new view.
    Fork { from: String, name: String },
    /// Include changes, and the changes they depend on, in a view.
    Include { view: String, changes: Vec<String> },
    /// Take changes out of a view.
    Exclude {
        view: String,
        changes: Vec<String>,
        /// Also exclude the included changes that depend on them.
        #[arg(long)]
        cascade: bool,
    },
    /// What differs between two scopes (b defaults to canon).
    Compare { a: String, b: Option<String> },
    /// List the open views.
    Ls {
        /// Include closed views.
        #[arg(long)]
        all: bool,
        /// Flag views idle after this many days (default: views.idle_days).
        #[arg(long)]
        stale: Option<u32>,
    },
    /// A view's changes, conflicts and recent ops.
    Show { view: String },
    /// Close views, or with --stale the views idle that many days. Human only for --stale.
    Close {
        views: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        stale: Option<u32>,
    },
    /// Hand a view to another session (actor[/run]), or leave it unowned. Human only.
    Own {
        view: String,
        owner: Option<String>,
        #[arg(long)]
        none: bool,
    },
}

/// The `ViewRequest` JSON for `cmd`.
pub fn args_of(cmd: &ViewCmd) -> Value {
    match cmd {
        ViewCmd::New { name, base, pin } => {
            json!({"op": "create", "name": name, "base": base, "pin": pin})
        }
        ViewCmd::Fork { from, name } => json!({"op": "fork", "from": from, "name": name}),
        ViewCmd::Include { view, changes } => {
            json!({"op": "include", "view": view, "changes": changes})
        }
        ViewCmd::Exclude {
            view,
            changes,
            cascade,
        } => json!({"op": "exclude", "view": view, "changes": changes, "cascade": cascade}),
        ViewCmd::Compare { a, b } => json!({"op": "compare", "a": a, "b": b}),
        ViewCmd::Ls { all, stale } => json!({"op": "list", "all": all, "stale": stale}),
        ViewCmd::Show { view } => json!({"op": "show", "view": view}),
        ViewCmd::Close {
            views,
            reason,
            stale,
        } => {
            // an empty `views` would close nothing: `--stale` is the other way to name views
            let views = (!views.is_empty()).then_some(views);
            json!({"op": "close", "views": views, "reason": reason, "stale": stale})
        }
        ViewCmd::Own { view, owner, none } => {
            json!({"op": "own", "view": view, "owner": owner, "none": none})
        }
    }
}

/// The `ViewRequest` JSON for `rdy undo`; no `view` means the session's own.
pub fn undo_args(view: Option<String>, op: Option<String>, cascade: bool) -> Value {
    json!({"op": "undo", "view": view, "target": op, "cascade": cascade})
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

/// `actor` or `actor/run`, from an `{actor, run}` object.
fn owner_text(o: &Value) -> String {
    match o["run"].as_str() {
        Some(run) => format!("{}/{run}", text(&o["actor"])),
        None => text(&o["actor"]).to_string(),
    }
}

fn count(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// `view {name} ({n} changes, base {short|none}{, pinned}{, owner {actor[/run]}})`.
fn head_line(h: &Value) -> String {
    let mut line = format!(
        "view {} ({} changes, base {}",
        text(&h["view"]),
        num(&h["changes"]),
        h["base"].as_str().map_or("none", short)
    );
    if h["pinned"] == true {
        line.push_str(", pinned");
    }
    if h["owner"].is_object() {
        line.push_str(&format!(", owner {}", owner_text(&h["owner"])));
    }
    line.push(')');
    line
}

/// One `conflict {block}: {n} sides` line per conflict, each on its own line after `prefix`.
fn conflict_lines(conflicts: &Value, indent: &str) -> String {
    conflicts
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "\n{indent}conflict {}: {} sides",
                text(&c["block"]),
                count(&c["sides"])
            )
        })
        .collect()
}

/// How long ago `ms` (epoch milliseconds) was, in the largest whole unit.
fn age(ms: i64) -> String {
    if ms <= 0 {
        return "-".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let secs = ((now - ms) / 1000).max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn compare_text(v: &Value) -> String {
    let mut lines = vec![format!("{} vs {}", text(&v["a"]), text(&v["b"]))];
    let entries = |key: &str, f: &dyn Fn(&Value) -> String, lines: &mut Vec<String>| {
        for e in v[key].as_array().into_iter().flatten() {
            lines.push(f(e));
        }
    };
    let with_path = |sign: &str, e: &Value| {
        let mut l = format!("{sign} {} {}", text(&e["id"]), text(&e["block_type"]));
        if let Some(p) = e["path"].as_str() {
            l.push_str(&format!(" {p}"));
        }
        l
    };
    entries("added", &|e| with_path("+", e), &mut lines);
    entries("deleted", &|e| with_path("-", e), &mut lines);
    entries(
        "changed",
        &|e| {
            let fields: Vec<&str> = e["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!(
                "~ {} {} {}",
                text(&e["id"]),
                text(&e["block_type"]),
                fields.join(",")
            )
        },
        &mut lines,
    );
    entries(
        "moved",
        &|e| {
            format!(
                "> {} {} {} -> {}",
                text(&e["id"]),
                text(&e["block_type"]),
                e["from_parent"].as_str().unwrap_or("-"),
                e["to_parent"].as_str().unwrap_or("-")
            )
        },
        &mut lines,
    );
    entries(
        "conflicted",
        &|e| {
            format!(
                "! {} conflict {}",
                text(&e["id"]),
                short(text(&e["conflict"]))
            )
        },
        &mut lines,
    );
    if lines.len() == 1 {
        lines.push("no differences".into());
    }
    lines.join("\n")
}

fn ls_text(v: &Value) -> String {
    let mut rows = vec![
        [
            "VIEW",
            "OWNER",
            "BASE",
            "CHANGES",
            "BLOCKS",
            "CONFLICTS",
            "LAST WRITE",
            "FLAGS",
        ]
        .map(String::from),
    ];
    for x in v["views"].as_array().into_iter().flatten() {
        let mut flags = vec![];
        for (key, word) in [("pinned", "pinned"), ("closed", "closed"), ("idle", "idle")] {
            if x[key] == true {
                flags.push(word);
            }
        }
        rows.push([
            text(&x["name"]).to_string(),
            if x["owner"].is_object() {
                owner_text(&x["owner"])
            } else {
                "-".into()
            },
            x["effective_base"]
                .as_str()
                .or(x["base"].as_str())
                .map_or("-", short)
                .to_string(),
            num(&x["changes"]).to_string(),
            num(&x["blocks"]).to_string(),
            num(&x["conflicts"]).to_string(),
            age(x["last_write_ms"].as_i64().unwrap_or(0)),
            flags.join(","),
        ]);
    }
    let widths: Vec<usize> = (0..8)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|r| {
            let cells: Vec<String> = r
                .iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect();
            cells.join("  ").trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn show_text(v: &Value) -> String {
    let mut out = head_line(&v["head"]);
    out.push_str("\nchanges:");
    for c in v["changes"].as_array().into_iter().flatten() {
        let message = text(&c["message"]);
        let blocks: Vec<&str> = c["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        out.push_str(&format!(
            "\n  {} {} {} [{}]",
            short(text(&c["id"])),
            text(&c["actor"]),
            if message.is_empty() { "-" } else { message },
            blocks.join(", ")
        ));
    }
    out.push_str("\nconflicts:");
    out.push_str(&conflict_lines(&v["conflicts"], "  "));
    out.push_str("\nops:");
    for o in v["ops"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n  {} {}{} {}",
            short(text(&o["id"])),
            text(&o["kind"]),
            if o["undone"] == true { " (undone)" } else { "" },
            text(&o["actor"])
        ));
    }
    out
}

fn close_text(v: &Value) -> String {
    let closed = v["closed"].as_array().map_or(&[][..], Vec::as_slice);
    if closed.is_empty() {
        return "closed 0 views".into();
    }
    closed
        .iter()
        .map(|c| match text(&c["reason"]) {
            "" => format!("closed {}", text(&c["view"])),
            reason => format!("closed {} ({reason})", text(&c["view"])),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of the `view` op's reply to a request of kind `op` (`create`, `fork`, `include`,
/// `exclude`, `compare`, `list`, `show`, `close`, `own` or `undo`).
pub fn render(op: &str, reply: &Value) -> String {
    match op {
        "create" | "fork" => head_line(reply),
        "include" => {
            let mut out = format!(
                "included {} change(s) into {}",
                count(&reply["added"]),
                text(&reply["view"])
            );
            if count(&reply["added_deps"]) > 0 {
                out.push_str(&format!(" (+{} dependencies)", count(&reply["added_deps"])));
            }
            out.push_str(&conflict_lines(&reply["conflicts"], ""));
            out
        }
        "exclude" => {
            let mut out = format!(
                "excluded {} change(s) from {}",
                count(&reply["removed"]),
                text(&reply["view"])
            );
            if count(&reply["cascaded"]) > 0 {
                out.push_str(&format!(" (+{} dependents)", count(&reply["cascaded"])));
            }
            out
        }
        "compare" => compare_text(reply),
        "list" => ls_text(reply),
        "show" => show_text(reply),
        "close" => close_text(reply),
        "own" => format!(
            "view {} owner {}",
            text(&reply["view"]),
            if reply["owner"].is_object() {
                owner_text(&reply["owner"])
            } else {
                "none".into()
            }
        ),
        "undo" => {
            let mut out = format!(
                "undid {} {}: {}",
                text(&reply["kind"]),
                short(text(&reply["undone"])),
                text(&reply["effect"])
            );
            if reply["rebased"] == true {
                out.push_str(", rebased onto canon");
            }
            out.push_str(&conflict_lines(&reply["conflicts"], ""));
            out
        }
        _ => serde_json::to_string_pretty(reply).unwrap_or_default(),
    }
}

/// The line `rdy show` appends for a conflicted block (`block` is the show reply): the version
/// the block reads as, and the change behind each side.
pub fn conflict_text(block: &Value) -> String {
    let c = &block["conflict"];
    let sides: Vec<String> = c["sides"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| match s["change"].as_str() {
            Some(change) => {
                let summary = text(&s["summary"]);
                let actor = summary.split_once(": ").map_or(summary, |(a, _)| a);
                format!("{} by {actor}", short(change))
            }
            None => "canon".to_string(),
        })
        .collect();
    format!(
        "conflict {}: reads as base {}; sides: {}",
        short(text(&c["id"])),
        c["base"].as_str().map_or("none", short),
        sides.join(", ")
    )
}
