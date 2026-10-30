use crate::conn::{Env, call};
use crate::output::{carried_targets, hash_short, num, open_warnings, text};
use crate::{history_text, view_cmd, wc};
use rdy_daemon::ErrorReply;
use rdy_schema::CoverageDimension;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct ShowArgs {
    /// An id or a path such as wallets/transactions; `id@version` or a ref line `[id@version] …` reads under that version. P-<id>@<n> shows a proposal version. A landing id (the full hash or at least 6 hex characters), landing:<id> or tag:<version> shows that landing.
    id: String,
    /// canon (the default), an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long)]
    scope: Option<String>,
    /// Also list every version: its change, author, landing and approver.
    #[arg(long)]
    blame: bool,
}

#[derive(clap::Args)]
pub(crate) struct LogArgs {
    /// At most this many landings, newest first.
    #[arg(long)]
    limit: Option<usize>,
    /// Only landings after this one: a landing id (at least 6 hex characters), landing:<id> or tag:<version>.
    #[arg(long)]
    since: Option<String>,
    /// Check every signature offline and show the report under each landing and tag. Needs no SSH agent or hub.
    #[arg(long)]
    verify: bool,
}

#[derive(clap::Args)]
pub(crate) struct ImpactArgs {
    /// An id or a path such as wallets/transactions.
    id: String,
    #[arg(long, default_value_t = 3)]
    depth: u32,
    /// canon, an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
}

#[derive(clap::Args)]
pub(crate) struct TraceArgs {
    /// An id or a path such as wallets/transactions; `id@version` or a ref line `[id@version] …` reads under that version.
    id: String,
    /// canon (the default), an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long)]
    scope: Option<String>,
    /// How many steps of lineage to walk, 1 to 8 (default 3).
    #[arg(long, allow_negative_numbers = true)]
    depth: Option<i64>,
    /// Add the recorded evidence: authored options, sealed plans, landing evidence and the whole re-affirmation record.
    #[arg(long)]
    detail: bool,
}

#[derive(clap::Args)]
pub(crate) struct LsArgs {
    path: Option<String>,
    /// canon, an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
    #[arg(long)]
    depth: Option<u32>,
}

#[derive(clap::Args)]
pub(crate) struct SuspectsArgs {
    /// canon (the default), an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long)]
    scope: Option<String>,
}

pub(crate) fn status(env: &Env) -> Result<(Value, String), ErrorReply> {
    let mut v = call(env, "status", json!({}))?;
    let mut t = status_text(&v);
    wc::status::attach(env, &mut v, &mut t)?;
    Ok((v, t))
}

pub(crate) fn show(env: &Env, a: ShowArgs) -> Result<(Value, String), ErrorReply> {
    let ShowArgs { id, scope, blame } = a;
    let v = call(
        env,
        "show",
        json!({"id": id, "scope": scope, "blame": blame}),
    )?;
    let mut t = show_text(&v);
    if blame {
        t.push_str(&history_text::blame_text(&v));
    }
    Ok((v, t))
}

pub(crate) fn log(env: &Env, a: LogArgs) -> Result<(Value, String), ErrorReply> {
    let LogArgs {
        limit,
        since,
        verify,
    } = a;
    let args = if verify {
        json!({"limit": limit, "since": since, "verify": true})
    } else {
        json!({"limit": limit, "since": since})
    };
    let v = call(env, "log", args)?;
    let t = log_text(&v);
    Ok((v, t))
}

pub(crate) fn impact(env: &Env, a: ImpactArgs) -> Result<(Value, String), ErrorReply> {
    let ImpactArgs { id, depth, scope } = a;
    let v = call(
        env,
        "impact",
        json!({"id": id, "depth": depth, "scope": scope}),
    )?;
    let t = impact_text(&v);
    Ok((v, t))
}

/// `rdy trace` (D-1876T): the reply of the `trace` op, printed as `trace_text` reads it. The
/// JSON is the `rdy_trace` result. The budget is the tool's 1000 tokens.
pub(crate) fn trace(env: &Env, a: TraceArgs) -> Result<(Value, String), ErrorReply> {
    let TraceArgs {
        id,
        scope,
        depth,
        detail,
    } = a;
    let mut args = json!({"id": id, "scope": scope, "depth": depth});
    if detail {
        args["detail"] = json!(true);
    }
    let v = call(env, "trace", args)?;
    let t = crate::trace_text::trace_text(&v, detail);
    Ok((v, t))
}

pub(crate) fn ls(env: &Env, a: LsArgs) -> Result<(Value, String), ErrorReply> {
    let LsArgs { path, scope, depth } = a;
    let v = call(
        env,
        "ls",
        json!({"path": path, "scope": scope, "depth": depth}),
    )?;
    let mut lines = vec![];
    ls_text(&v, 0, &mut lines);
    Ok((v, lines.join("\n")))
}

pub(crate) fn suspects(env: &Env, a: SuspectsArgs) -> Result<(Value, String), ErrorReply> {
    let SuspectsArgs { scope } = a;
    let v = call(env, "suspects", json!({"scope": scope}))?;
    let t = suspects_text(&v);
    Ok((v, t))
}

pub(crate) fn tags(env: &Env) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "tags", json!({}))?;
    let t = tags_text(&v);
    Ok((v, t))
}

fn ls_text(nodes: &Value, depth: usize, out: &mut Vec<String>) {
    for n in nodes.as_array().into_iter().flatten() {
        let block_type = text(&n["type"]);
        let mut line = format!(
            "{}{}{}  {}  {}",
            "  ".repeat(depth),
            text(&n["name"]),
            if block_type == "folder" { "/" } else { "" },
            block_type,
            text(&n["id"])
        );
        if let Some(t) = n["doc_type"].as_str() {
            line.push_str(&format!("  {t}"));
        }
        if let Some(l) = n["lifecycle"].as_str() {
            line.push_str(&format!(" · {l}"));
        }
        if num(&n["collapsed"]) > 0 {
            line.push_str(&format!("  (+{} more)", num(&n["collapsed"])));
        }
        out.push(line);
        ls_text(&n["children"], depth + 1, out);
    }
}

/// The `coverage:`, `kinds:` and `similar kinds:` lines of `rdy status` (FW-CH3a, FW-EN1a), each
/// with its leading newline; nothing when the status carries no coverage.
fn coverage_lines(c: &Value) -> String {
    if !c.is_object() {
        return String::new();
    }
    let dims: Vec<String> = CoverageDimension::ALL
        .iter()
        .filter_map(|d| {
            c["dimensions"][d.name()]
                .as_u64()
                .map(|s| format!("{} {s}", d.name()))
        })
        .collect();
    // a project with nothing to score has no dimensions: no empty parentheses
    let dims = if dims.is_empty() {
        String::new()
    } else {
        format!(" ({})", dims.join(", "))
    };
    let mut out = format!(
        "\ncoverage: {}{dims}; {} coverage issues, listed by rdy coverage",
        num(&c["score"]),
        num(&c["issues_total"]),
    );
    let kinds: Vec<String> = c["kinds"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| format!("{} {}", text(&k["kind"]), num(&k["entities"])))
        .collect();
    if !kinds.is_empty() {
        out.push_str(&format!("\nkinds: {}", kinds.join(", ")));
    }
    let groups: Vec<String> = c["similar_kinds"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            g.as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect();
    if !groups.is_empty() {
        out.push_str(&format!("\nsimilar kinds: {}", groups.join("; ")));
    }
    out
}

/// The lines after the pack line (ec10, D-DAGNB): `repo <id>`, then the origin when there is
/// one: `cloned from <id> at version <tag>`, or `… at landing <12 hex digits>` when it has no
/// tag, or `cloned from <id>` with neither; a fork says `forked from`. Each ends in a newline.
pub(crate) fn repo_lines(v: &Value) -> String {
    let mut out = format!("repo {}\n", text(&v["repo_id"]));
    let (word, o) = if v["forked_from"].is_object() {
        ("forked from", &v["forked_from"])
    } else if v["origin"].is_object() {
        ("cloned from", &v["origin"])
    } else {
        return out;
    };
    out.push_str(&format!("{word} {}", text(&o["repo_id"])));
    if let Some(tag) = o["tag"].as_str() {
        out.push_str(&format!(" at version {tag}"));
    } else if let Some(landing) = o["landing"].as_str() {
        out.push_str(&format!(
            " at landing {}",
            landing.chars().take(12).collect::<String>()
        ));
    }
    out.push('\n');
    out
}

fn status_text(v: &Value) -> String {
    let views = v["views"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            format!(
                "  {}: {}",
                text(&w["name"]),
                text(&w["version_position"]["label"])
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!(
        "pack {}\n{}canon: {} ({} landings, {} suspects){}\nviews:\n{views}",
        text(&v["pack"]),
        repo_lines(v),
        text(&v["version_position"]["label"]),
        num(&v["landings"]),
        num(&v["suspects"]),
        coverage_lines(&v["coverage"]),
    );
    if let Some(e) = v["coverage_error"].as_str() {
        out.push_str(&format!("\ncoverage not read: {e}"));
    }
    let caught_up = num(&v["open_report"]["caught_up_frames"]);
    if caught_up > 0 {
        out.push_str(&format!("\nopen: caught up {caught_up} frames"));
    }
    out.push_str(&warning_lines_with_targets(
        &v["warnings"],
        &v["redaction_warnings"],
    ));
    out.push_str(&open_warnings(v));
    out
}

/// The warning lines, each carried-removal warning followed by its targets. The carried
/// warnings are in `warnings` as strings and, in the same order, in `carried` as structured
/// records: each is printed once, from the string.
fn warning_lines_with_targets(warnings: &Value, carried: &Value) -> String {
    let mut targets = carried.as_array().into_iter().flatten();
    let mut out = String::new();
    for w in warnings.as_array().into_iter().flatten() {
        out.push_str(&format!("\nwarning: {}", text(w)));
        if text(w).starts_with("W_STOP_BEFORE_REDACTION: ")
            && let Some(c) = targets.next()
        {
            out.push_str(&carried_targets(c));
        }
    }
    out
}

fn show_text(v: &Value) -> String {
    if v["landing"].is_object() {
        // dq7: a landing's row, then its change list
        return format!(
            "{}\n{}",
            log_text(&json!([v["landing"]])).trim_end_matches('\n'),
            crate::diff_text::diff_text(&v["diff"], false)
        );
    }
    if v["proposal"].is_object() {
        // the printer ends every output with a newline; proposal_text already does
        return rdy_engine::proposal_text(&v["proposal"])
            .trim_end_matches('\n')
            .to_string();
    }
    let status = v["status_words"].as_str();
    let mut fields = v["fields"].clone();
    if status.is_some()
        && let Some(f) = fields.as_object_mut()
    {
        // the stored status ("accepted") would contradict the derived one
        f.remove("status");
    }
    let mut out = format!(
        "{} {} (version {})\n",
        text(&v["id"]),
        text(&v["block_type"]),
        hash_short(text(&v["version"]))
    );
    if let Some(s) = status {
        out.push_str(&format!("status: {s}\n"));
    }
    if let Some(line) = history_text::last_change_line(&v["history"][0]) {
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(from) = v["effective_from"].as_str() {
        out.push_str(&format!("effective from: {from}\n"));
    }
    // trk1: the type's tracking fields, which stay writable once the decision is decided
    if let Some(names) = v["tracking"].as_array().filter(|a| !a.is_empty()) {
        let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
        out.push_str(&format!("tracking: {}\n", names.join(", ")));
    }
    out.push_str(&format!(
        "{}\n{}",
        serde_json::to_string_pretty(&fields).unwrap_or_default(),
        text(&v["body"])
    ));
    // rdx13 (Task 6): a stub names the request that removed its content, as the source text
    // does after `Content removed.`
    if let Some(h) = v["redaction"]["id"].as_str() {
        out.push_str(&format!("\n\nRedaction: {h}."));
    }
    if v["conflict"].is_object() {
        out.push('\n');
        out.push_str(&view_cmd::conflict_text(v));
    }
    // a block's suspects: structural rows, their accepted deferrals, and code rows
    if let Some(rows) = v["suspects"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("\nsuspects:");
        for row in rows {
            let lines = suspect_lines(row, |i| {
                format!(
                    "{} -> {}  {}",
                    text(&i["block"]),
                    text(&i["target"]),
                    text(&i["reason"])
                )
            });
            for line in lines {
                out.push_str(&format!("\n  {line}"));
            }
        }
    }
    // cgr1: in the inflight scope, the in-flight changes that write the block
    if let Some(rows) = v["inflight"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("\nin flight:");
        for c in rows {
            let who = rdy_engine::identity::actor_run_text(text(&c["actor"]), c["run"].as_str());
            let views: Vec<&str> = c["views"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut line = format!(
                "\n  {} {who} in {}",
                hash_short(text(&c["change"])),
                views.join(", ")
            );
            if let Some(i) = c["intent"].as_str() {
                line.push_str(&format!("; intent {i}"));
            }
            if c["alternative"].is_object() {
                line.push_str(&format!(
                    "; alternative {} of {}",
                    text(&c["alternative"]["member"]),
                    text(&c["alternative"]["set"])
                ));
            }
            let proposals: Vec<&str> = c["proposals"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !proposals.is_empty() {
                line.push_str(&format!("; proposed in {}", proposals.join(", ")));
            }
            out.push_str(&line);
        }
    }
    out
}

fn log_text(v: &Value) -> String {
    let rows = v.as_array().map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return "no landings".to_string();
    }
    let mut out = String::new();
    for row in rows {
        out.push_str(&history_text::log_head_line(row));
        out.push('\n');
        // malt1: the alternative the landing chose
        if let Some(member) = row["chosen"]["member"].as_str() {
            out.push_str(&format!(
                "    chose {member} of {}\n",
                text(&row["chosen"]["set"])
            ));
        }
        let (ev, relay) = (&row["evidence"], &row["relay_evidence"]);
        if ev.is_object() || relay.is_object() {
            let mut line = format!("    evidence ({})", text(&ev["kind"]));
            if relay.is_object() {
                line.push_str(&format!(": \"{}\"", text(&relay["quoted"])));
                match relay["question"].as_str() {
                    Some(q) => line.push_str(&format!(
                        " (asked: \"{q}\"; session {})",
                        text(&relay["session_ref"])
                    )),
                    None => line.push_str(&format!(" (session {})", text(&relay["session_ref"]))),
                }
            } else if let Some(c) = ev["choice"].as_str() {
                line.push_str(&format!(": chose \"{c}\""));
            }
            out.push_str(&line);
            out.push('\n');
        }
        // D-ME2HD: the assessment the landing recorded (none for a re-affirmation landing)
        if let Some(a) = row["assessment"].as_str() {
            out.push_str(&format!("    assessment {}\n", hash_short(a)));
        }
        // O23: the policy version a policy landing was accepted under
        if row["policy"].is_object() {
            out.push_str(&format!(
                "    policy {}@{}\n",
                text(&row["policy"]["block"]),
                hash_short(text(&row["policy"]["version"]))
            ));
        }
        // D-ZWP7Q: the dispute state of the landing, when it has one (O22)
        let state = text(&row["dispute"]["state"]);
        if !state.is_empty() && state != "none" {
            let n = row["dispute"]["disputes"].as_array().map_or(0, Vec::len);
            out.push_str(&format!("    dispute {state}: {n}\n"));
        }
        let from: Vec<&str> = row["rebased_from"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(hash_short)
            .collect();
        if !from.is_empty() {
            out.push_str(&format!("    rebased from {}\n", from.join(", ")));
        }
        // sig9: a verified row carries its signature reports
        if row.get("signatures").is_some() {
            for line in rdy_engine::signing_read::verified_lines(row) {
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    out
}

fn impact_text(v: &Value) -> String {
    let mut lines: Vec<String> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            format!(
                "depth {}  {} {}: {}  [{} {}]",
                num(&i["depth"]),
                text(&i["id"]),
                text(&i["block_type"]),
                text(&i["summary"]),
                text(&i["via"]["kind"]),
                text(&i["via"]["from"])
            )
        })
        .collect();
    if let Some(areas) = v["by_area"].as_object().filter(|a| !a.is_empty()) {
        let parts: Vec<String> = areas
            .iter()
            .map(|(n, c)| format!("{n} ({})", num(c)))
            .collect();
        lines.push(format!("areas: {}", parts.join(", ")));
    }
    lines.join("\n")
}

fn suspects_text(v: &Value) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return "no suspects".to_string();
    }
    items
        .iter()
        .flat_map(|i| {
            suspect_lines(i, |i| {
                format!(
                    "{} -> {}  {}",
                    text(&i["block"]),
                    text(&i["target"]),
                    text(&i["reason"])
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lines of one suspect row (`rdy_suspects`, a context's `suspects`, a briefing's). A
/// structural row prints the line `head` gives, then `  deferred by <actor>: <reason>` when a
/// person accepted its deferral (the actor is who deferred it, not who accepted it). A code row
/// (null target) prints `<block>: code_changed`, then `  <repo>:<path>` for each path.
pub(crate) fn suspect_lines(row: &Value, head: impl FnOnce(&Value) -> String) -> Vec<String> {
    if row["target"].is_null() {
        let mut lines = vec![format!("{}: {}", text(&row["block"]), text(&row["reason"]))];
        lines.extend(
            row["paths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|p| format!("  {p}")),
        );
        return lines;
    }
    let mut lines = vec![head(row)];
    if row["deferred"].is_object() {
        lines.push(format!(
            "  deferred by {}: {}",
            text(&row["deferred"]["by"]["actor"]),
            text(&row["deferred"]["reason"])
        ));
    }
    lines
}

fn tags_text(v: &Value) -> String {
    let versions = v["versions"].as_array().cloned().unwrap_or_default();
    if versions.is_empty() {
        return "no versions yet".to_string();
    }
    let current = v["current"].as_str();
    versions
        .iter()
        .map(|t| {
            let mut line = format!("{}  {}", text(&t["name"]), hash_short(text(&t["landing"])));
            if let Some(m) = t["message"].as_str() {
                line.push_str(&format!("  {m}"));
            }
            if Some(text(&t["name"])) == current {
                line.push_str("  (current)");
            }
            // rdx13 (Task 6): the requests that removed content in the version's history
            let removed: Vec<&str> = t["redactions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !removed.is_empty() {
                line.push_str(&format!("  redacted by {}", removed.join(", ")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{log_text, show_text, status_text, suspects_text, tags_text};
    use serde_json::json;

    /// rdx13 (Task 6): a source read of a stub names the request that removed its content, as
    /// the source text does (`Redaction: {hash}.` after `Content removed.`); a version whose
    /// history holds removed content lists its requests. Other blocks and versions print as
    /// before.
    #[test]
    fn rdx13_source_reads_name_the_redaction() {
        let h = "ab".repeat(32);
        let v = json!({"id": "p-AAAAA", "block_type": "prose", "version": "cd".repeat(32),
            "fields": {}, "body": "Content removed.", "redaction": {"id": h}});
        let t = show_text(&v);
        assert!(
            t.ends_with(&format!("{{}}\nContent removed.\n\nRedaction: {h}.")),
            "{t}"
        );
        let mut plain = v.clone();
        plain.as_object_mut().unwrap().remove("redaction");
        assert!(show_text(&plain).ends_with("{}\nContent removed."));
        let tags = json!({"current": "v2", "versions": [
            {"name": "v1", "landing": "12".repeat(32)},
            {"name": "v2", "landing": "34".repeat(32), "message": "m", "redacted": true,
             "redactions": ["ab".repeat(32), "cd".repeat(32)]}
        ]});
        assert_eq!(
            tags_text(&tags),
            format!(
                "v1  121212121212\nv2  343434343434  m  (current)  redacted by {}, {}",
                "ab".repeat(32),
                "cd".repeat(32)
            )
        );
    }

    /// cp13: the suspects text keeps the structural line, follows it with the deferring author's
    /// line, and prints a code row with one line per path.
    #[test]
    fn cp13_suspects_text_shows_deferrals_and_code_rows() {
        let v = json!({"items": [
            {"block": "c-B", "target": "c-A", "reason": "changed",
             "deferred": {"by": {"actor": "agent:w", "run": null, "role": "Writer", "at_ms": 1},
                          "reason": "the release owns it", "accepted_by": "human:alice"}},
            {"block": "c-B", "target": "c-X", "reason": "changed"},
            {"block": "c-B", "target": null, "reason": "code_changed",
             "paths": ["app:src/a.rs", "app:src/c.rs"]},
            {"block": "c-C", "target": null, "reason": "code_changed", "paths": []}
        ]});
        assert_eq!(
            suspects_text(&v),
            "c-B -> c-A  changed\n  deferred by agent:w: the release owns it\nc-B -> c-X  changed\nc-B: code_changed\n  app:src/a.rs\n  app:src/c.rs\nc-C: code_changed"
        );
        assert_eq!(suspects_text(&json!({"items": []})), "no suspects");
        // the block text prints the same rows, indented under a heading
        let shown = json!({"id": "c-B", "block_type": "claim", "version": "ab".repeat(32),
            "fields": {}, "body": "B.", "suspects": [
            {"block": "c-B", "target": "c-A", "reason": "changed",
             "deferred": {"by": {"actor": "agent:w"}, "reason": "later", "accepted_by": "h"}},
            {"block": "c-B", "target": null, "reason": "code_changed", "paths": ["app:src/a.rs"]}]});
        assert!(
            show_text(&shown).ends_with(
                "\nsuspects:\n  c-B -> c-A  changed\n    deferred by agent:w: later\n  c-B: code_changed\n    app:src/a.rs"
            ),
            "{}",
            show_text(&shown)
        );
    }

    /// cc15: a coverage read error is printed in place of the coverage lines.
    #[test]
    fn cc15_status_text_says_when_the_coverage_was_not_read() {
        let v = json!({
            "pack": "p", "landings": 0, "suspects": 0, "views": [],
            "version_position": {"label": "v0"}, "warnings": [],
            "coverage_error": "E_INTERNAL: boom"
        });
        let t = status_text(&v);
        assert!(t.contains("\ncoverage not read: E_INTERNAL: boom"), "{t}");
        assert!(!t.contains("coverage: "), "{t}");
    }

    /// Final-review fix round 1: `rdy log` prints the alternative a landing chose, as `rdy
    /// approve` does.
    #[test]
    fn malt1_log_text_prints_the_chosen_alternative() {
        let row = json!({"landing": "ab".repeat(32), "at": "2026-10-07T00:00:00Z", "title": "x",
            "proposed_by": {"actor": "agent:a"}, "approved_by": "human:qa",
            "chosen": {"set": "as-0123456789ab", "member": "a"}});
        let t = log_text(&json!([row]));
        assert!(t.contains("\n    chose a of as-0123456789ab\n"), "{t}");
        let mut plain = row.clone();
        plain.as_object_mut().unwrap().remove("chosen");
        assert!(!log_text(&json!([plain])).contains("chose"));
    }

    /// ap7 (O23, D-ZWP7Q): `rdy log` prints the policy line right after the assessment line, then
    /// the dispute line when the state is not `none`.
    #[test]
    fn ap7_log_text_prints_the_policy_and_dispute_lines() {
        let row = json!({"landing": "ab".repeat(32), "at": "2026-10-07T00:00:00Z", "title": "x",
            "proposed_by": {"actor": "agent:a"}, "approved_by": "agent:a",
            "assessment": "cd".repeat(32),
            "policy": {"block": "ap-00001", "version": "ef".repeat(32)},
            "dispute": {"state": "open", "disputes": ["12".repeat(32), "34".repeat(32)]}});
        let t = log_text(&json!([row]));
        let want = format!(
            "    assessment {}\n    policy ap-00001@{}\n    dispute open: 2\n",
            "cd".repeat(6),
            "ef".repeat(6)
        );
        assert!(t.contains(&want), "{t}");
        // no policy and no dispute: neither line
        let mut plain = row.clone();
        plain["policy"] = json!(null);
        plain["dispute"] = json!({"state": "none", "disputes": []});
        let t = log_text(&json!([plain]));
        assert!(!t.contains("policy") && !t.contains("dispute"), "{t}");
    }

    /// Final-review fix round 1: `rdy show --scope inflight` prints the in-flight changes that
    /// write the block, as the JSON lists them.
    #[test]
    fn cgr1_show_text_prints_the_inflight_changes() {
        let v = json!({"id": "c-AAAAA", "block_type": "claim", "version": "cd".repeat(32),
        "fields": {}, "body": "b", "inflight": [
            {"change": "ab".repeat(32), "views": ["a", "b"], "actor": "agent:a", "run": "ra",
             "intent": "in-0123456789ab", "alternative": {"set": "as-0123456789ab", "member": "x"},
             "proposals": ["P-0123456789ab@1"]},
            {"change": "ef".repeat(32), "views": ["c"], "actor": "human:qa", "run": null,
             "proposals": []}
        ]});
        let t = show_text(&v);
        assert!(
            t.ends_with("\nin flight:\n  abababababab agent:a (run ra) in a, b; intent in-0123456789ab; alternative x of as-0123456789ab; proposed in P-0123456789ab@1\n  efefefefefef human:qa in c"),
            "{t}"
        );
        // absent (another scope, or no in-flight change writes the block): no block
        let mut none = v.clone();
        none.as_object_mut().unwrap().remove("inflight");
        assert!(!show_text(&none).contains("in flight"));
    }
}
