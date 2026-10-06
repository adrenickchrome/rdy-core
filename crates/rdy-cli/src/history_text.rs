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

/// `actor`, or `actor/run` when the run is set and is not the human's `cli`.
pub(crate) fn who(author: &Value) -> String {
    match author["run"].as_str() {
        Some(run) if run != rdy_engine::HUMAN_RUN => format!("{}/{run}", s(author, "actor")),
        _ => s(author, "actor").to_string(),
    }
}

/// `ms` as RFC 3339 UTC to the second.
pub(crate) fn time(ms: &Value) -> String {
    rdy_engine::rfc3339(ms.as_i64().unwrap_or(0))
}

/// `implements <id>` or `editorial <class>: <reason>`; `None` for no justification.
pub(crate) fn justification_text(j: &Value) -> Option<String> {
    if let Some(id) = j["implements"].as_str() {
        return Some(format!("implements {id}"));
    }
    let e = &j["editorial"];
    e.is_object()
        .then(|| format!("editorial {}: {}", s(e, "class"), s(e, "reason")))
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
    if let Some(j) = justification_text(&entry["justification"]) {
        line.push_str(&format!(", {j}"));
    }
    line.push_str(&landed_suffix(entry));
    Some(line)
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
        match e["landing"]["id"].as_str() {
            Some(id) => out.push_str(&format!(
                "\n    landed {}, approved by {} at {}",
                short(id),
                s(&e["landing"], "approved_by"),
                time(&e["landing"]["at_ms"])
            )),
            None => out.push_str("\n    not landed"),
        }
    }
    if reply["history_more"].as_bool() == Some(true) {
        out.push_str("\n  … older versions not shown");
    }
    out
}
