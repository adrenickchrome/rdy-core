use crate::conn::{Env, call};
use crate::output::{hash_short, num, open_warnings, text, warning_lines};
use crate::{history_text, view_cmd, wc};
use rdy_daemon::ErrorReply;
use rdy_schema::CoverageDimension;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct ShowArgs {
    /// An id or a path such as wallets/transactions; `id@version` or a ref line `[id@version] …` reads under that version.
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
    let LogArgs { limit, since } = a;
    let v = call(env, "log", json!({"limit": limit, "since": since}))?;
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
    out.push_str(&warning_lines(&v["warnings"]));
    out.push_str(&open_warnings(v));
    out
}

fn show_text(v: &Value) -> String {
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
    if v["conflict"].is_object() {
        out.push('\n');
        out.push_str(&view_cmd::conflict_text(v));
    }
    // cgr1: in the inflight scope, the in-flight changes that write the block
    if let Some(rows) = v["inflight"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("\nin flight:");
        for c in rows {
            let who = match c["run"].as_str() {
                Some(run) => format!("{}/{run}", text(&c["actor"])),
                None => text(&c["actor"]).to_string(),
            };
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
        .map(|i| {
            format!(
                "{} -> {}  {}",
                text(&i["block"]),
                text(&i["target"]),
                text(&i["reason"])
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{log_text, show_text, status_text};
    use serde_json::json;

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
            t.ends_with("\nin flight:\n  abababababab agent:a/ra in a, b; intent in-0123456789ab; alternative x of as-0123456789ab; proposed in P-0123456789ab@1\n  efefefefefef human:qa in c"),
            "{t}"
        );
        // absent (another scope, or no in-flight change writes the block): no block
        let mut none = v.clone();
        none.as_object_mut().unwrap().remove("inflight");
        assert!(!show_text(&none).contains("in flight"));
    }
}
