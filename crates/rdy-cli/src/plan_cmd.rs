//! `rdy plan`, `rdy reaffirm` and `rdy repin` (D-4JMPH, D-EJBPD, D-01QX5, D-KQ4CA): each builds
//! the request for the ctl op of the same name, and the renderers turn the reply into text. The
//! checks are the engine's; a refusal comes back unchanged. `--json` prints the engine's reply
//! as it is (for `repin`, the write's report alone, as `rdy_write` gives it).
use crate::conn::{Env, call};
use crate::edit_cmd::write_text;
use crate::output::{num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

/// The kinds of a plan item, in the order a read lists them.
const KINDS: [&str; 6] = [
    "update",
    "rejustify",
    "reaffirm",
    "confirm",
    "update_implementation",
    "tension",
];

/// The outcomes of a sealed item, in the order a read lists them.
const OUTCOMES: [&str; 11] = [
    "updated",
    "relinked",
    "repinned",
    "reaffirmed",
    "retired",
    "committed",
    "confirmed",
    "done",
    "deferred",
    "open",
    "dismissed",
];

/// The usage refusal of an answer that names no item and no group.
const NO_ITEM_NO_GROUP: &str = "name an item, or a group with --kind, --follow-up, --doc or --edge";

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct PlanArgs {
    #[command(subcommand)]
    answer: Option<AnswerCmd>,
    /// Read the plan of this view (default: the bound view, else your own).
    #[arg(long)]
    view: Option<String>,
    /// Read the plan of this proposal version, P-<id>@<n>.
    #[arg(long)]
    proposal: Option<String>,
    /// Read the plan a landing sealed: its full hash.
    #[arg(long)]
    landing: Option<String>,
    /// Read every seal that names this decision.
    #[arg(long)]
    decision: Option<String>,
    /// The budget for the reply; the items are cut to fit.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
    /// Continue a cut plan: the next cursor it printed. Alone (--max-tokens allowed).
    #[arg(long)]
    page: Option<String>,
}

/// Answers to plan items, recorded together or not at all.
#[derive(clap::Subcommand)]
enum AnswerCmd {
    /// The item holds as it is. Needs --reason.
    Confirm(AnswerArgs),
    /// Leave the item for later. Needs --reason.
    Defer(AnswerArgs),
    /// The work the item names is done. A reason is not kept.
    Done(AnswerArgs),
    /// Dismiss a tension item, with a reason.
    Dismiss(AnswerArgs),
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("by_group").multiple(true).args(["kind", "follow_up", "doc", "edge"])))]
pub(crate) struct AnswerArgs {
    /// The items: pi-<hex> ids (a unique prefix is enough), tn- ids or cq- ids.
    items: Vec<String>,
    /// Why (1 to 2000 characters).
    #[arg(long)]
    reason: Option<String>,
    /// Answer every open item of this kind (update, rejustify, reaffirm, confirm, update_implementation or tension).
    #[arg(long)]
    kind: Option<String>,
    /// Answer every open item of this follow-up: update, reaffirm or confirm.
    #[arg(long = "follow-up")]
    follow_up: Option<String>,
    /// Answer every open item whose subject is in this doc.
    #[arg(long)]
    doc: Option<String>,
    /// Answer every open item with this edge.
    #[arg(long)]
    edge: Option<String>,
    /// With a confirm group: also repin each matched subject to its cause, in one change.
    #[arg(long, requires = "by_group")]
    repin: bool,
    /// The view whose plan is answered (default: the bound view, else your own).
    #[arg(long)]
    view: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ReaffirmArgs {
    /// The suspect decisions that still hold.
    ids: Vec<String>,
    /// Instead of ids: every decision in the view that is suspect on this block.
    #[arg(long)]
    because: Option<String>,
    /// Why they still hold (1 to 2000 characters).
    #[arg(long)]
    reason: String,
    /// Only the suspects on these blocks (default: every current suspect).
    #[arg(long, value_delimiter = ',')]
    targets: Option<Vec<String>>,
    /// The view to write in (default: the bound view, else your own).
    #[arg(long)]
    view: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct RepinArgs {
    /// The block whose references and links are repinned.
    id: String,
    /// Only these targets (default: every block it cites).
    #[arg(long, value_delimiter = ',')]
    targets: Option<Vec<String>>,
    /// Also record the code its reflects globs match now.
    #[arg(long)]
    code: bool,
    /// The view to write in (default: the bound view, else your own).
    #[arg(long)]
    view: Option<String>,
}

pub(crate) fn plan(env: &Env, a: PlanArgs) -> Result<(Value, String), ErrorReply> {
    let PlanArgs {
        answer,
        view,
        proposal,
        landing,
        decision,
        max_tokens,
        page,
    } = a;
    let Some(answer) = answer else {
        let v = call(
            env,
            "plan",
            json!({"view": view, "proposal": proposal, "landing": landing,
                "decision": decision, "max_tokens": max_tokens, "page": page}),
        )?;
        let t = plan_text(&v);
        return Ok((v, t));
    };
    let (act, a) = match answer {
        AnswerCmd::Confirm(a) => ("confirm", a),
        AnswerCmd::Defer(a) => ("defer", a),
        AnswerCmd::Done(a) => ("done", a),
        AnswerCmd::Dismiss(a) => ("dismiss", a),
    };
    let mut group = serde_json::Map::new();
    for (key, value) in [
        ("kind", &a.kind),
        ("follow_up", &a.follow_up),
        ("doc", &a.doc),
        ("edge", &a.edge),
    ] {
        if let Some(v) = value {
            group.insert(key.into(), json!(v));
        }
    }
    if a.items.is_empty() && group.is_empty() {
        clap::Error::raw(
            clap::error::ErrorKind::MissingRequiredArgument,
            NO_ITEM_NO_GROUP,
        )
        .exit();
    }
    let mut answers: Vec<Value> = a
        .items
        .iter()
        .map(|item| json!({"item": item, "act": act, "reason": a.reason}))
        .collect();
    if !group.is_empty() {
        let mut spec = json!({"group": group, "act": act, "reason": a.reason});
        if a.repin {
            spec["repin"] = json!(true);
        }
        answers.push(spec);
    }
    let v = call(env, "plan", json!({"view": a.view, "answers": answers}))?;
    let mut lines: Vec<String> = a
        .items
        .iter()
        .map(|item| format!("answered {item}: {act}"))
        .collect();
    if !group.is_empty() {
        lines.push(format!("answered group: {act}"));
    }
    Ok((v, lines.join("\n")))
}

pub(crate) fn reaffirm(env: &Env, a: ReaffirmArgs) -> Result<(Value, String), ErrorReply> {
    let ReaffirmArgs {
        ids,
        because,
        reason,
        targets,
        view,
    } = a;
    let v = call(
        env,
        "reaffirm",
        json!({"ids": (!ids.is_empty()).then_some(ids), "because": because,
            "targets": targets, "reason": reason, "view": view}),
    )?;
    let mut t = reaffirmed_text(&v);
    t.push_str(&write_text(&v));
    Ok((v, t))
}

pub(crate) fn repin(env: &Env, a: RepinArgs) -> Result<(Value, String), ErrorReply> {
    let RepinArgs {
        id,
        targets,
        code,
        view,
    } = a;
    let reply = call(
        env,
        "repin",
        json!({"id": id, "targets": targets, "code": code, "view": view}),
    )?;
    let t = format!(
        "repinned {id}: {} pin(s)\n{}",
        num(&reply["pins"]),
        write_text(&reply["result"])
    );
    Ok((reply["result"].clone(), t))
}

/// The `reaffirmed <id> (after <cause>, ...)` line of each decision a write re-affirmed, with a
/// line break after each; the write's `reaffirmed` list is sorted by decision, and its causes too.
fn reaffirmed_text(v: &Value) -> String {
    v["reaffirmed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let causes: Vec<&str> = r["causes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            format!(
                "reaffirmed {} (after {})\n",
                text(&r["id"]),
                causes.join(", ")
            )
        })
        .collect()
}

/// A live or sealed item as one line: `<id> <kind> <subject>: <state> (<cause>)`, with
/// `<outcome>` for a sealed item. A live item whose answer has a reason is followed by it on a
/// line of its own, indented by two spaces.
pub(crate) fn item_text(item: &Value) -> String {
    let state = item["state"]
        .as_str()
        .or_else(|| item["outcome"].as_str())
        .unwrap_or("open");
    let mut line = format!(
        "{} {} {}: {} ({})",
        text(&item["id"]),
        text(&item["kind"]),
        text(&item["subject"]),
        state,
        text(&item["cause"])
    );
    if let Some(reason) = item["answer"]["reason"].as_str() {
        line.push_str(&format!("\n  {reason}"));
    }
    line
}

/// A plan reply as text: one entry per row (an item, or a seal that names a decision with its
/// nonzero outcomes), then a line per kind, the code line and, on a cut page, the cursor.
pub(crate) fn plan_text(v: &Value) -> String {
    let mut lines: Vec<String> = vec![];
    for row in v["items"].as_array().into_iter().flatten() {
        if row.get("landing").is_some() {
            lines.push(format!(
                "{} {}: {} items",
                text(&row["landing"]),
                text(&row["version"]),
                num(&row["items"])
            ));
            for outcome in OUTCOMES {
                let n = num(&row["outcomes"][outcome]);
                if n > 0 {
                    lines.push(format!("  {outcome}: {n}"));
                }
            }
        } else {
            lines.push(item_text(row));
        }
    }
    for kind in KINDS {
        let c = &v["counts"]["by_kind"][kind];
        lines.push(format!(
            "{kind}: {} open, {} closed, {} deferred",
            num(&c["open"]),
            num(&c["closed"]),
            num(&c["deferred"])
        ));
    }
    if v["code"]["unavailable"].as_bool() == Some(true) {
        lines.push("code: unavailable".into());
    } else {
        lines.push(format!("code: {} files", num(&v["code"]["files"])));
    }
    if let Some(next) = v["next"].as_str() {
        lines.push(format!("next: {next}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O17: the rows, the six kind lines in enum order, the code line and the cursor.
    #[test]
    fn cp11_plan_text_has_the_closed_lines() {
        let item = |id: &str, kind: &str, answer: Value| {
            json!({"id": id, "kind": kind, "subject": "c-BBBBB", "cause": "c-AAAAA",
                "edge": "references", "state": "deferred", "answer": answer})
        };
        let v = json!({
            "scope": "view:w", "plan": null,
            "items": [
                item("pi-1", "confirm", json!({"act": "defer", "reason": "later", "by": {}})),
                item("pi-2", "update", Value::Null),
                {"id": "pi-3", "kind": "update", "subject": "c-C", "cause": "c-A",
                    "edge": "references", "outcome": "repinned"},
                {"landing": "L1", "version": "V1", "items": 3,
                    "outcomes": {"open": 1, "updated": 2, "done": 0}},
            ],
            "counts": {"open": 0, "closed": 0, "deferred": 2, "by_kind": {
                "update": {"open": 1, "closed": 2, "deferred": 3},
                "rejustify": {"open": 0, "closed": 0, "deferred": 0},
                "reaffirm": {"open": 0, "closed": 0, "deferred": 0},
                "confirm": {"open": 4, "closed": 5, "deferred": 6},
                "update_implementation": {"open": 0, "closed": 0, "deferred": 0},
                "tension": {"open": 0, "closed": 0, "deferred": 0}}},
            "code": {"files": 7},
            "notes": [],
            "next": "plan1:2:00:view:w",
        });
        assert_eq!(
            plan_text(&v),
            "pi-1 confirm c-BBBBB: deferred (c-AAAAA)\n  later\n\
pi-2 update c-BBBBB: deferred (c-AAAAA)\n\
pi-3 update c-C: repinned (c-A)\n\
L1 V1: 3 items\n  updated: 2\n  open: 1\n\
update: 1 open, 2 closed, 3 deferred\n\
rejustify: 0 open, 0 closed, 0 deferred\n\
reaffirm: 0 open, 0 closed, 0 deferred\n\
confirm: 4 open, 5 closed, 6 deferred\n\
update_implementation: 0 open, 0 closed, 0 deferred\n\
tension: 0 open, 0 closed, 0 deferred\n\
code: 7 files\n\
next: plan1:2:00:view:w"
        );
        let mut v = v;
        v["code"] = json!({"files": 0, "unavailable": true});
        v["next"] = Value::Null;
        assert!(plan_text(&v).ends_with("code: unavailable"));
    }

    /// p-0SH30: one line per decision, its sorted causes joined by a comma and a space.
    #[test]
    fn cp11_reaffirmed_lines_join_the_causes() {
        let v = json!({"reaffirmed": [
            {"id": "D-AAAAA", "causes": ["c-1", "c-2"]},
            {"id": "D-BBBBB", "causes": ["c-1"]}]});
        assert_eq!(
            reaffirmed_text(&v),
            "reaffirmed D-AAAAA (after c-1, c-2)\nreaffirmed D-BBBBB (after c-1)\n"
        );
        assert_eq!(reaffirmed_text(&json!({})), "");
    }
}
