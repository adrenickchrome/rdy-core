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
    Show {
        view: String,
        /// Print every block id of each change (default: the count and the first 10).
        #[arg(long)]
        all: bool,
    },
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
        ViewCmd::Show { view, .. } => json!({"op": "show", "view": view}),
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

/// How many block ids `rdy view show` prints per change without `--all`.
pub const SHOW_BLOCKS: usize = 10;

/// A change's block ids: every one when `all` or there are at most `SHOW_BLOCKS`, else the
/// count, the first `SHOW_BLOCKS` and how many more.
fn blocks_text(blocks: &[&str], all: bool) -> String {
    let n = blocks.len();
    if all || n <= SHOW_BLOCKS {
        return format!("[{}]", blocks.join(", "));
    }
    format!(
        "[{n} blocks: {}, … +{} more (--all)]",
        blocks[..SHOW_BLOCKS].join(", "),
        n - SHOW_BLOCKS
    )
}

fn show_text(v: &Value, all: bool) -> String {
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
            "\n  {} {} {} {}",
            short(text(&c["id"])),
            text(&c["actor"]),
            if message.is_empty() { "-" } else { message },
            blocks_text(&blocks, all)
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
    let mut lines: Vec<String> = closed
        .iter()
        .map(|c| match text(&c["reason"]) {
            "" => format!("closed {}", text(&c["view"])),
            reason => format!("closed {} ({reason})", text(&c["view"])),
        })
        .collect();
    if lines.is_empty() {
        lines.push("closed 0 views".into());
    }
    // `close --stale` keeps the session's own default view open
    if let Some(kept) = v["kept"].as_str() {
        lines.push(format!("kept {kept} open (your default view)"));
    }
    lines.join("\n")
}

/// The text of the `view` op's reply to `cmd`.
pub fn render_view(cmd: &ViewCmd, reply: &Value) -> String {
    match cmd {
        ViewCmd::Show { all, .. } => show_text(reply, *all),
        _ => render(text(&args_of(cmd)["op"]), reply),
    }
}

/// The text of the `view` op's reply to a request of kind `op` (`create`, `fork`, `include`,
/// `exclude`, `compare`, `list`, `show`, `close`, `own` or `undo`). Include, exclude and undo
/// end with their consequences.
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
            out.push_str(&crate::brief_text::consequences_text(reply));
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
            out.push_str(&crate::brief_text::consequences_text(reply));
            out
        }
        "compare" => compare_text(reply),
        "list" => ls_text(reply),
        "show" => show_text(reply, false),
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
            out.push_str(&crate::brief_text::consequences_text(reply));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn detail(n: usize) -> Value {
        let blocks: Vec<String> = (0..n).map(|i| format!("c-{i:05}")).collect();
        json!({
            "head": {"view": "big", "head": "ab".repeat(32), "base": null, "owner": null, "pinned": false, "closed": false, "changes": 1},
            "effective_base": null,
            "changes": [{"id": "cd".repeat(32), "actor": "human:qa", "run": null, "at_ms": 1, "message": "bulk", "blocks": blocks}],
            "blocks": n, "conflicts": [], "ops": []
        })
    }

    #[test]
    fn vshw1_show_prints_the_count_and_the_first_ten_blocks_of_a_large_change() {
        let t = show_text(&detail(15), false);
        let line = t.lines().find(|l| l.contains("bulk")).unwrap();
        assert!(
            line.ends_with("[15 blocks: c-00000, c-00001, c-00002, c-00003, c-00004, c-00005, c-00006, c-00007, c-00008, c-00009, … +5 more (--all)]"),
            "{line}"
        );
    }

    #[test]
    fn vshw1_show_all_and_a_small_change_list_every_block() {
        let all = show_text(&detail(15), true);
        assert!(
            all.contains("c-00014]") && !all.contains("(--all)"),
            "{all}"
        );
        let small = show_text(&detail(10), false);
        assert!(
            small.contains("[c-00000, c-00001")
                && small.contains("c-00009]")
                && !small.contains("blocks:"),
            "{small}"
        );
    }

    #[test]
    fn vshw1_include_exclude_and_undo_print_their_consequences() {
        let cq = json!([
            {"id": "cq-1", "block": "c-B", "kind": "suspect", "because": {"edge": "references", "changed": "c-A"}, "follow_up": "update"},
            {"id": "cq-2", "block": "c-A", "kind": "conflict", "because": {"edge": "conflict", "changed": "c-A"}, "follow_up": "resolve", "conflict": "ee", "fix": "write the block"}
        ]);
        let want = "\nconsequences:\n  suspect c-B: update (references c-A)\n  conflict c-A: resolve (conflict c-A)\n    fix: write the block";
        for (op, reply) in [
            (
                "include",
                json!({"view": "r1", "added": ["x"], "added_deps": [], "conflicts": [], "consequences": cq}),
            ),
            (
                "exclude",
                json!({"view": "r1", "removed": ["x"], "cascaded": [], "consequences": cq}),
            ),
            (
                "undo",
                json!({"kind": "include", "undone": "ab".repeat(32), "effect": "excluded 1", "rebased": false, "conflicts": [], "consequences": cq}),
            ),
        ] {
            let t = render(op, &reply);
            assert!(t.ends_with(want), "{op}: {t}");
        }
    }
}
