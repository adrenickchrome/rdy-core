//! The text of `rdy diff`, `rdy q` and `rdy show <landing>` (O16, O17): one line per entry or match.
use crate::output::{hash_short, num, text};
use serde_json::Value;

/// A scope label as a person can copy it as a scope (O16).
fn label_text(l: &Value) -> String {
    match text(&l["kind"]) {
        "canon" => "canon".to_string(),
        "view" => format!("view:{}", text(&l["name"])),
        "tag" => format!("tag:{}", text(&l["name"])),
        "landing" => format!("landing:{}", hash_short(text(&l["landing"]))),
        "proposal" => format!("proposal:{}", text(&l["name"])),
        "inflight" => "inflight".to_string(),
        _ => "the empty project".to_string(),
    }
}

/// A field value on one side: its JSON, or `(none)` when the side lacks the field.
fn side_text(f: &Value, side: &str) -> String {
    f.get(side).map_or("(none)".to_string(), Value::to_string)
}

/// The ids of a list, joined with `, ` and a sign in front of each.
fn signed(sign: char, ids: &Value) -> Vec<String> {
    ids.as_array()
        .into_iter()
        .flatten()
        .map(|id| format!("{sign}{}", text(id)))
        .collect()
}

/// A body's pieces as one text: `[-removed-]` and `{+added+}` around the words, each line break
/// continuing on a new line indented 8 spaces.
fn body_text(pieces: &Value) -> String {
    let mut out = String::new();
    for p in pieces.as_array().into_iter().flatten() {
        let t = text(&p["text"]);
        match text(&p["op"]) {
            "del" => out.push_str(&format!("[-{t}-]")),
            "add" => out.push_str(&format!("{{+{t}+}}")),
            _ => out.push_str(t),
        }
    }
    out.replace('\n', "\n        ")
}

/// The diff reply as text (O16). `detail` prints field values and the word diff of each body.
pub(crate) fn diff_text(v: &Value, detail: bool) -> String {
    let mut head = format!("{} to {}", label_text(&v["from"]), label_text(&v["to"]));
    if let Some(root) = v["root"].as_str() {
        head.push_str(&format!(" in {root}"));
    }
    let c = &v["counts"];
    head.push_str(&format!(
        ": {} added, {} removed, {} updated, {} moved",
        num(&c["added"]),
        num(&c["removed"]),
        num(&c["updated"]),
        num(&c["moved"])
    ));
    let mut out = vec![head];
    let entries = v["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if entries.is_empty() {
        out.push("no changes".to_string());
    }
    for e in entries {
        let place = e["path"].as_str().or(e["doc"].as_str()).unwrap_or("-");
        out.push(format!(
            "{} {} ({}) {}: {}",
            text(&e["change"]),
            text(&e["id"]),
            text(&e["block_type"]),
            place,
            text(&e["summary"])
        ));
        if e["type"].is_object() {
            out.push(format!(
                "    type {} to {}",
                text(&e["type"]["from"]),
                text(&e["type"]["to"])
            ));
        }
        if e["moved"].is_object() {
            let m = &e["moved"];
            let mut line = format!(
                "    moved from {} to {}",
                m["from_path"].as_str().unwrap_or("-"),
                m["to_path"].as_str().unwrap_or("-")
            );
            if m["reordered_only"].as_bool() == Some(true) {
                line.push_str(" (order only)");
            }
            out.push(line);
        }
        let fields = e["fields"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if detail {
            for f in fields {
                out.push(format!(
                    "    field {}: {} to {}",
                    text(&f["name"]),
                    side_text(f, "from"),
                    side_text(f, "to")
                ));
            }
        } else if !fields.is_empty() {
            let names: Vec<&str> = fields.iter().map(|f| text(&f["name"])).collect();
            out.push(format!("    fields {}", names.join(", ")));
        }
        for l in e["links"].as_array().into_iter().flatten() {
            let mut ids = signed('+', &l["added"]);
            ids.extend(signed('-', &l["removed"]));
            out.push(format!("    links {} {}", text(&l["rel"]), ids.join(", ")));
        }
        if e["decision_status"].is_object() {
            out.push(format!(
                "    status {} to {}",
                text(&e["decision_status"]["from"]),
                text(&e["decision_status"]["to"])
            ));
        }
        let pins: Vec<&str> = e["repinned"]
            .as_array()
            .into_iter()
            .flatten()
            .map(text)
            .collect();
        if !pins.is_empty() {
            out.push(format!("    repinned {}", pins.join(", ")));
        }
        if detail && e["body"].is_array() {
            out.push(format!("    body {}", body_text(&e["body"])));
        }
    }
    out.join("\n")
}

/// The query reply as text (O17): the count, then one line per match.
pub(crate) fn query_text(v: &Value) -> String {
    let total = num(&v["total"]);
    let mut out = vec![if total == 1 {
        "1 matching block".to_string()
    } else {
        format!("{total} matching blocks")
    }];
    for m in v["matches"].as_array().into_iter().flatten() {
        out.push(format!(
            "{}  {} ({})  {}",
            m["path"].as_str().unwrap_or("-"),
            text(&m["id"]),
            text(&m["block_type"]),
            text(&m["summary"])
        ));
    }
    out.join("\n")
}
