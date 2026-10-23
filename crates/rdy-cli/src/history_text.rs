//! blame1: the text of a block's history, as `rdy show` prints it.
use serde_json::Value;

/// A string field of `v`, empty when absent.
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// The first 12 characters of a hash.
fn short(h: &str) -> &str {
    &h[..h.len().min(12)]
}

/// `<actor> (run <run>)` when the run is set and is not the human's `cli`, else the actor
/// alone (D-PWB82: the actor and the run are never joined into one string).
pub(crate) fn who(author: &Value) -> String {
    let run = author["run"]
        .as_str()
        .filter(|run| *run != rdy_engine::HUMAN_RUN);
    rdy_engine::identity::actor_run_text(s(author, "actor"), run)
}

/// `ms` as RFC 3339 UTC to the second; `?` when the reply has no integer time.
pub(crate) fn time(ms: &Value) -> String {
    ms.as_i64().map_or_else(|| "?".into(), rdy_engine::rfc3339)
}

/// logt1: the first line of a `rdy log` entry: the landing, when, the title, who proposed it and
/// who approved it (and relayed the approval), and its tags.
pub(crate) fn log_head_line(row: &Value) -> String {
    let mut line = format!(
        "{}  {}  {}  proposed by {}  ",
        short(s(row, "landing")),
        row["at"].as_str().unwrap_or("?"),
        s(row, "title"),
        who(&row["proposed_by"])
    );
    match row["approved"].as_str() {
        Some(a) => line.push_str(&format!("approved {a} by {}", s(row, "approved_by"))),
        None => line.push_str(&format!("approved by {}", s(row, "approved_by"))),
    }
    if row["relayed_by"].is_object() {
        line.push_str(&format!(", relayed by {}", who(&row["relayed_by"])));
    }
    let tags: Vec<&str> = row["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !tags.is_empty() {
        line.push_str(&format!("  [{}]", tags.join(", ")));
    }
    line
}

/// `implements <id>, <id>`, `editorial <class>: <reason>`, or `reflects <id>, <id>` with
/// `, code` appended when code is set (`reflects code` alone for code only); `None` for no
/// justification.
pub(crate) fn justification_text(j: &Value) -> Option<String> {
    if let Some(ids) = j["implements"].as_array() {
        let ids: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
        return Some(format!("implements {}", ids.join(", ")));
    }
    let r = &j["reflects"];
    if r.is_object() {
        let mut parts: Vec<&str> = r["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if r["code"].as_bool() == Some(true) {
            parts.push("code");
        }
        return Some(format!("reflects {}", parts.join(", ")));
    }
    let e = &j["editorial"];
    e.is_object()
        .then(|| format!("editorial {}: {}", s(e, "class"), s(e, "reason")))
}

/// `imported by <who>` for a change `rdy import` wrote; `None` for any other change.
fn imported_by(entry: &Value) -> Option<String> {
    let i = &entry["imported_by"];
    i.is_object().then(|| format!("imported by {}", who(i)))
}

/// `, landed <id>` or `, not landed`.
fn landed_suffix(entry: &Value) -> String {
    match entry["landing"]["id"].as_str() {
        Some(id) => format!(", landed {}", short(id)),
        None => ", not landed".into(),
    }
}

/// The `last change:` line for a block's newest history entry; `None` when there is none.
pub(crate) fn last_change_line(entry: &Value) -> Option<String> {
    if !entry.is_object() {
        return None;
    }
    let a = &entry["author"];
    let mut line = format!(
        "last change: {} by {} at {}",
        short(s(entry, "change")),
        who(a),
        time(&a["at_ms"])
    );
    if let Some(i) = imported_by(entry) {
        line.push_str(&format!(", {i}"));
    }
    if let Some(j) = justification_text(&entry["justification"]) {
        line.push_str(&format!(", {j}"));
    }
    line.push_str(&landed_suffix(entry));
    if let Some(r) = removed_by(entry) {
        line.push_str(&format!("; {r}"));
    }
    Some(line)
}

/// rdx13 (Task 6): `content removed by redaction <hash>` for an entry whose version reads as a
/// stub, naming the request by its full hash; `None` for any other entry.
fn removed_by(entry: &Value) -> Option<String> {
    entry["redaction"]
        .as_str()
        .map(|h| format!("content removed by redaction {h}"))
}

/// The `history:` section of `rdy show --blame`, starting with a newline.
pub(crate) fn blame_text(reply: &Value) -> String {
    let mut out = String::from("\nhistory:");
    for e in reply["history"].as_array().into_iter().flatten() {
        let a = &e["author"];
        out.push_str(&format!(
            "\n  {}  {}  {}  {}  {}",
            short(s(e, "version")),
            s(e, "op"),
            short(s(e, "change")),
            who(a),
            time(&a["at_ms"])
        ));
        if let Some(j) = justification_text(&e["justification"]) {
            out.push_str(&format!("  {j}"));
        }
        if let Some(i) = imported_by(e) {
            out.push_str(&format!("  {i}"));
        }
        if let Some(r) = removed_by(e) {
            out.push_str(&format!("\n    {r}"));
        }
        match e["landing"]["id"].as_str() {
            Some(id) => {
                out.push_str(&format!(
                    "\n    landed {}, approved by {} at {}",
                    short(id),
                    s(&e["landing"], "approved_by"),
                    time(&e["landing"]["at_ms"])
                ));
                if e["landing"]["relayed_by"].is_object() {
                    let r = who(&e["landing"]["relayed_by"]);
                    out.push_str(&format!(", relayed by {r}"));
                }
            }
            None => out.push_str("\n    not landed"),
        }
    }
    if reply["history_more"].as_bool() == Some(true) {
        out.push_str("\n  … older versions not shown");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blame1_a_missing_time_prints_a_placeholder_not_the_epoch() {
        // fix round 1 (M4): a malformed reply reads as malformed
        assert_eq!(time(&Value::Null), "?");
        assert_eq!(time(&json!("soon")), "?");
        assert_eq!(time(&json!(0)), "1970-01-01T00:00:00Z");
        let row = json!({"landing": "abcdef0123456789", "title": "T", "approved_by": "human:qa",
            "proposed_by": {"actor": "agent:a", "run": "ra"}});
        assert_eq!(
            log_head_line(&row),
            "abcdef012345  ?  T  proposed by agent:a (run ra)  approved by human:qa"
        );
    }

    fn entry(imported_by: Option<Value>) -> Value {
        let mut e = json!({
            "version": "aaaaaaaaaaaa1111", "op": "update", "change": "0123456789abcdef",
            "author": {"actor": "agent:a", "run": "r1", "at_ms": 1_791_540_000_000_i64},
            "justification": {"implements": ["d-1"]},
            "landing": {"id": "fedcba9876543210", "approved_by": "human:qa", "at_ms": 1_791_540_000_000_i64}
        });
        if let Some(i) = imported_by {
            e["imported_by"] = i;
        }
        e
    }

    #[test]
    fn ib13_the_last_change_line_names_the_importer() {
        let plain = last_change_line(&entry(None)).unwrap();
        assert!(!plain.contains("imported by"), "{plain}");
        let run =
            last_change_line(&entry(Some(json!({"actor": "human:imp", "run": "r9"})))).unwrap();
        assert!(
            run.contains("at 2026-10-09T10:00:00Z")
                && run.contains(
                    ", imported by human:imp (run r9), implements d-1, landed fedcba987654"
                ),
            "{run}"
        );
        let human =
            last_change_line(&entry(Some(json!({"actor": "human:imp", "run": null})))).unwrap();
        assert!(
            human.contains(", imported by human:imp, implements"),
            "{human}"
        );
        let cli =
            last_change_line(&entry(Some(json!({"actor": "human:imp", "run": "cli"})))).unwrap();
        assert!(cli.contains(", imported by human:imp, implements"), "{cli}");
        let mut bare = entry(Some(json!({"actor": "human:imp", "run": null})));
        bare["justification"] = Value::Null;
        assert!(
            last_change_line(&bare)
                .unwrap()
                .ends_with("Z, imported by human:imp, landed fedcba987654")
        );
    }

    #[test]
    fn ib13_blame_names_the_importer() {
        let reply = json!({"history": [entry(None)]});
        assert!(!blame_text(&reply).contains("imported by"));
        let reply = json!({"history": [entry(Some(json!({"actor": "human:imp", "run": "r9"})))]});
        let text = blame_text(&reply);
        let first = text.lines().nth(2).unwrap();
        assert!(
            first.ends_with("implements d-1  imported by human:imp (run r9)"),
            "{first}"
        );
        let reply = json!({"history": [entry(Some(json!({"actor": "human:imp", "run": null})))]});
        assert!(blame_text(&reply).contains("  imported by human:imp\n"));
    }

    /// rdx13 (Task 6): a history entry whose version reads as a stub names the request that
    /// removed its content, in the last-change line and in blame; others print as before.
    #[test]
    fn rdx13_history_names_the_removing_request() {
        let h = "ef".repeat(32);
        let mut e = entry(None);
        let plain_line = last_change_line(&e).unwrap();
        let plain_blame = blame_text(&json!({"history": [e.clone()]}));
        e["redaction"] = json!(h);
        assert_eq!(
            last_change_line(&e).unwrap(),
            format!("{plain_line}; content removed by redaction {h}")
        );
        let blame = blame_text(&json!({"history": [e]}));
        let mut lines: Vec<&str> = plain_blame.lines().collect();
        let line = format!("    content removed by redaction {h}");
        lines.insert(3, &line);
        assert_eq!(blame, lines.join("\n"));
    }

    /// rf4 (D-4WC6R): a change justified by reflects prints as its label does, from the wire
    /// form the engine writes: its sources in stored order, then `code` when it is set.
    #[test]
    fn rf4_cli_last_change_and_blame_show_reflects() {
        let wire = |sources: &[&str], code: bool| {
            serde_json::to_value(rdy_engine::JustificationInfo::Reflects {
                sources: sources.iter().map(|s| s.to_string()).collect(),
                code,
            })
            .unwrap()
        };
        for (j, text) in [
            (
                wire(&["S-0000B", "S-0000A"], true),
                "reflects S-0000B, S-0000A, code",
            ),
            (wire(&[], true), "reflects code"),
            (wire(&["S-0000A"], false), "reflects S-0000A"),
        ] {
            assert_eq!(justification_text(&j).as_deref(), Some(text));
            let mut e = entry(None);
            e["justification"] = j;
            assert_eq!(
                last_change_line(&e).unwrap(),
                format!(
                    "last change: 0123456789ab by agent:a (run r1) at 2026-10-09T10:00:00Z, {text}, \
                     landed fedcba987654"
                )
            );
            let blame = blame_text(&json!({"history": [e]}));
            let first = blame.lines().nth(2).unwrap();
            assert_eq!(
                first,
                format!(
                    "  aaaaaaaaaaaa  update  0123456789ab  agent:a (run r1)  2026-10-09T10:00:00Z  {text}"
                )
            );
        }
    }

    #[test]
    fn df7_the_blame_landing_line_ends_with_the_relayer() {
        let mut e = entry(None);
        let plain = blame_text(&json!({"history": [e.clone()]}));
        assert!(
            plain.ends_with("approved by human:qa at 2026-10-09T10:00:00Z"),
            "{plain}"
        );
        e["landing"]["relayed_by"] = json!({"actor": "agent:w", "run": "r1"});
        let relayed = blame_text(&json!({"history": [e.clone()]}));
        assert!(
            relayed.ends_with(
                "approved by human:qa at 2026-10-09T10:00:00Z, relayed by agent:w (run r1)"
            ),
            "{relayed}"
        );
        // final review M3: the object keeps a `cli` run; the text names the actor alone, as
        // `who` does everywhere
        e["landing"]["relayed_by"] = json!({"actor": "agent:w", "run": "cli"});
        let cli = blame_text(&json!({"history": [e]}));
        assert!(
            cli.ends_with("approved by human:qa at 2026-10-09T10:00:00Z, relayed by agent:w"),
            "{cli}"
        );
    }
}
