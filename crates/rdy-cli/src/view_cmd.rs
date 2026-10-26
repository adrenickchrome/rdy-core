//! cview1: `rdy view *` and `rdy undo`. The commands build one `ViewRequest` JSON for the ctl
//! `view` op; the renderers turn the op's reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
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
    /// Replay selected changes into your open view.
    Replay {
        view: String,
        #[arg(required = true, num_args = 1..)]
        changes: Vec<String>,
    },
    /// Take changes out of a view.
    Exclude {
        view: String,
        changes: Vec<String>,
        /// Also exclude the included changes that depend on them.
        #[arg(long)]
        cascade: bool,
    },
    /// Resolve a conflict on an accepted or rejected decision by taking the landed side: the
    /// view's block then reads canon's frozen version, and the change depends on every side.
    Resolve {
        view: String,
        /// The conflicted block: id, key or path.
        block: String,
        /// Take the landed side (required; the one side resolve takes).
        #[arg(long, required = true)]
        landed: bool,
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
    /// Close views, or with --stale the views idle that many days. --stale is a person's act:
    /// an agent session relays it with the person's words (--relayed-by and --evidence).
    Close {
        views: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        stale: Option<u32>,
        /// With --stale: the person's words, quoted, when an agent relays the act. Requires
        /// --relayed-by.
        #[arg(long, value_name = "WORDS", requires_all = ["stale", "relayed_by"])]
        evidence: Option<String>,
        /// The agent that relays the person's words: your own actor. The run comes from your
        /// session. Only an agent session relays. Requires --evidence.
        #[arg(long, value_name = "ACTOR", requires_all = ["stale", "evidence"])]
        relayed_by: Option<String>,
        /// The question the person was asked.
        #[arg(long, value_name = "TEXT", requires = "evidence")]
        question: Option<String>,
        /// Where the person said it (default: cli).
        #[arg(long = "session", value_name = "REF", requires = "evidence")]
        session_ref: Option<String>,
    },
    /// Hand a view to an actor and optional run, or leave it unowned. A person's act: an agent
    /// session relays it with the person's words (--relayed-by and --evidence).
    Own {
        view: String,
        /// The new owner's actor: human:<name>, agent:<name>[/<name>], or ci:<repo_id>/<name>.
        #[arg(value_name = "ACTOR")]
        owner: Option<String>,
        /// The owner's run. Omit it for an owner with no run.
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        none: bool,
        /// The person's words, quoted, when an agent relays the act. Requires --relayed-by.
        #[arg(long, value_name = "WORDS", requires = "relayed_by")]
        evidence: Option<String>,
        /// The agent that relays the person's words: your own actor. The run comes from your
        /// session. Only an agent session relays. Requires --evidence.
        #[arg(long, value_name = "ACTOR", requires = "evidence")]
        relayed_by: Option<String>,
        /// The question the person was asked.
        #[arg(long, value_name = "TEXT", requires = "evidence")]
        question: Option<String>,
        /// Where the person said it (default: cli).
        #[arg(long = "session", value_name = "REF", requires = "evidence")]
        session_ref: Option<String>,
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
        ViewCmd::Replay { view, changes } => {
            json!({"op": "replay", "view": view, "changes": changes})
        }
        ViewCmd::Exclude {
            view,
            changes,
            cascade,
        } => json!({"op": "exclude", "view": view, "changes": changes, "cascade": cascade}),
        ViewCmd::Resolve {
            view,
            block,
            landed,
        } => json!({"op": "resolve", "view": view, "block": block, "landed": landed}),
        ViewCmd::Compare { a, b } => json!({"op": "compare", "a": a, "b": b}),
        ViewCmd::Ls { all, stale } => json!({"op": "list", "all": all, "stale": stale}),
        ViewCmd::Show { view, .. } => json!({"op": "show", "view": view}),
        ViewCmd::Close {
            views,
            reason,
            stale,
            evidence,
            relayed_by,
            question,
            session_ref,
        } => {
            // an empty `views` would close nothing: `--stale` is the other way to name views
            let views = (!views.is_empty()).then_some(views);
            json!({"op": "close", "views": views, "reason": reason, "stale": stale,
                "evidence": evidence, "question": question, "session_ref": session_ref,
                "relayed_by": relayed_by})
        }
        ViewCmd::Own {
            view,
            owner,
            run,
            none,
            evidence,
            relayed_by,
            question,
            session_ref,
        } => {
            // the owner is an object, its run apart (D-PWB82); a run alone is refused in `view`
            let owner = owner
                .as_ref()
                .map(|actor| json!({"actor": actor, "run": run}));
            json!({"op": "own", "view": view, "owner": owner, "none": none,
                "evidence": evidence, "question": question, "session_ref": session_ref,
                "relayed_by": relayed_by})
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

/// `<actor> (run <run>)`, or the actor alone, from an `{actor, run}` object.
fn owner_text(o: &Value) -> String {
    rdy_engine::identity::actor_run_text(text(&o["actor"]), o["run"].as_str())
}

fn count(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// `view {name} ({n} changes, base {short|none}{, pinned}{, owner {actor}{ (run {run})}})`.
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

pub fn compare_text(v: &Value) -> String {
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
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| {
            let cells: Vec<String> = r
                .iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect();
            cells.join("  ").trim_end().to_string()
        })
        .collect();
    // why each closed view was closed (`--all`)
    for x in v["views"].as_array().into_iter().flatten() {
        if let Some(reason) = x["close_reason"].as_str() {
            lines.push(format!("closed {}: {reason}", text(&x["name"])));
        }
    }
    lines.join("\n")
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
            "\n  {} {} {}{} {}",
            short(text(&c["id"])),
            text(&c["actor"]),
            if message.is_empty() { "-" } else { message },
            if c["client_nonce"].is_string() {
                " [wc]"
            } else {
                ""
            },
            blocks_text(&blocks, all)
        ));
        if let Some(source) = c["replayed_from"].as_str() {
            out.push_str(&format!(" replayed from {}", short(source)));
        }
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
        if let Some(reason) = o["reason"].as_str() {
            out.push_str(&format!(": {reason}"));
        }
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
    lines.extend(relayed_line(v));
    lines.join("\n")
}

/// `relayed by <actor> (run <run>)`, or without the run clause when there is none, when the
/// reply names the relaying agent.
fn relayed_line(v: &Value) -> Option<String> {
    let by = v["relayed_by"]["actor"].as_str()?;
    Some(format!(
        "relayed by {}",
        rdy_engine::identity::actor_run_text(by, v["relayed_by"]["run"].as_str())
    ))
}

/// The text of the `view` op's reply to `cmd`.
pub fn render_view(cmd: &ViewCmd, reply: &Value) -> String {
    match cmd {
        ViewCmd::Show { all, .. } => show_text(reply, *all),
        _ => render(text(&args_of(cmd)["op"]), reply),
    }
}

/// The text of the `view` op's reply to a request of kind `op` (`create`, `fork`, `include`,
/// `exclude`, `replay`, `compare`, `list`, `show`, `close`, `own`, `resolve` or `undo`). Include,
/// exclude, replay and undo end with their consequences.
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
        "replay" => {
            let mut out = format!(
                "replayed {} change(s) into {}",
                count(&reply["replayed"]),
                text(&reply["view"])
            );
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
        "resolve" => format!(
            "resolved conflict {} on {} in {}: took the landed side {} (change {})",
            short(text(&reply["conflict"])),
            text(&reply["block"]),
            text(&reply["view"]),
            short(text(&reply["version"])),
            short(text(&reply["change"]))
        ),
        "compare" => compare_text(reply),
        "list" => ls_text(reply),
        "show" => show_text(reply, false),
        "close" => close_text(reply),
        "own" => {
            let mut out = format!(
                "view {} owner {}",
                text(&reply["view"]),
                if reply["owner"].is_object() {
                    owner_text(&reply["owner"])
                } else {
                    "none".into()
                }
            );
            if let Some(line) = relayed_line(reply) {
                out.push_str(&format!(", {line}"));
            }
            out
        }
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

#[derive(clap::Args)]
pub(crate) struct UndoArgs {
    /// The view (default: the session's own).
    #[arg(long)]
    view: Option<String>,
    /// The view op to undo (default: this session's newest undoable op).
    op: Option<String>,
    /// Also exclude the included changes that depend on the ones the undo excludes.
    #[arg(long)]
    cascade: bool,
}

pub(crate) fn undo(env: &Env, a: UndoArgs) -> Result<(Value, String), ErrorReply> {
    let UndoArgs { view, op, cascade } = a;
    let v = call(env, "view", undo_args(view, op, cascade))?;
    let t = render("undo", &v);
    Ok((v, t))
}

/// The refusal of `--run` with no owner: a run belongs to an owner's actor.
fn run_without_owner() -> ErrorReply {
    ErrorReply::new(
        rdy_engine::E_BAD_INPUT,
        "--run needs an owner",
        "pass an actor before --run, or omit --run",
    )
}

pub(crate) fn view(env: &Env, cmd: ViewCmd) -> Result<(Value, String), ErrorReply> {
    // the command line's own combinations, before the op: `--run` names an owner's run
    if let ViewCmd::Own {
        owner: None,
        run: Some(_),
        ..
    } = &cmd
    {
        return Err(run_without_owner());
    }
    let v = call(env, "view", args_of(&cmd))?;
    let t = render_view(&cmd, &v);
    Ok((v, t))
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

    /// Final-review fix round 1: a close op prints its reason, and `view ls --all` prints why
    /// each closed view was closed, under the table.
    #[test]
    fn vshw1_a_close_reason_prints_in_view_show_and_view_ls() {
        let mut d = detail(1);
        d["ops"] = json!([
            {"id": "cd".repeat(32), "kind": "close", "actor": "human:qa", "undone": false,
             "reason": "alternative chosen: as-0123456789ab chose a"},
            {"id": "ef".repeat(32), "kind": "commit", "actor": "agent:a", "undone": false}
        ]);
        let t = show_text(&d, false);
        assert!(
            t.contains(" close human:qa: alternative chosen: as-0123456789ab chose a\n"),
            "{t}"
        );
        assert!(t.trim_end().ends_with("commit agent:a"), "{t}");
        let ls = json!({"views": [
            {"name": "a", "owner": null, "base": null, "changes": 1, "blocks": 1, "conflicts": 0,
             "last_write_ms": 0, "pinned": false, "closed": false, "idle": false},
            {"name": "b", "owner": null, "base": null, "changes": 1, "blocks": 1, "conflicts": 0,
             "last_write_ms": 0, "pinned": false, "closed": true, "idle": false,
             "close_reason": "alternative chosen: as-0123456789ab chose a"}
        ]});
        let t = ls_text(&ls);
        assert!(
            t.ends_with("\nclosed b: alternative chosen: as-0123456789ab chose a"),
            "{t}"
        );
        assert_eq!(t.lines().count(), 4, "{t}");
    }
}
