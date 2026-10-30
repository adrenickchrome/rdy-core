use rdy_daemon::ErrorReply;
use serde_json::Value;
use std::fmt::Write as _;

pub(crate) fn hash_short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

/// The string at `v`, or "".
pub(crate) fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The number at `v`, or 0.
pub(crate) fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// The `warning:` lines for what the store's open left behind: quarantine files (`status` and
/// `fsck` replies). An unclean shutdown is no warning: the open recovered from it.
pub(crate) fn open_warnings(v: &Value) -> String {
    let mut out = String::new();
    let names: Vec<&str> = v["quarantine"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !names.is_empty() {
        out.push_str(&format!("\nwarning: quarantine: {}", names.join(", ")));
    }
    out
}

/// The target lines of one structured carried-removal warning, each after the warning's text.
pub(crate) fn carried_targets(w: &Value) -> String {
    w["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| format!("\n  target: {}", text(&i["target"])))
        .collect()
}

pub(crate) fn warning_lines(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|w| format!("\nwarning: {}", text(w)))
        .collect()
}

pub(crate) fn print_error(e: &ErrorReply, as_json: bool) {
    // the whole report, then one write that cannot panic (formatting into a String cannot fail)
    let mut out = String::new();
    if as_json {
        let _ = writeln!(out, "{}", serde_json::to_string(e).unwrap_or_default());
        write_stderr(&out);
        return;
    }
    let _ = writeln!(out, "error {}: {}\nfix: {}", e.code, e.message, e.fix);
    // an answer that names a card that moved, or an approval that waits for exceptions, carries
    // the fresh card or cards (O21): show them to the person
    if let Some(corrected) = e.extra.get("corrected") {
        let cards = corrected
            .get("exceptions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            // a fresh card (RULING F1) is an item: its text is `corrected.card.card`
            .chain(
                corrected
                    .get("card")
                    .map(|c| if c.is_object() { c } else { corrected }),
            );
        for c in cards {
            if let Some(card) = c.get("card").and_then(Value::as_str) {
                let _ = writeln!(out, "\n{card}");
            }
        }
    }
    if let Some(issues) = e.extra.get("issues").and_then(Value::as_array) {
        for i in issues {
            let block = match i["block"].as_str() {
                Some(b) => format!("Some({b})"),
                None => "None".to_string(),
            };
            let _ = writeln!(
                out,
                "  {} {block}: {} (fix: {})",
                text(&i["code"]),
                text(&i["message"]),
                text(&i["fix"])
            );
        }
    }
    for d in e
        .extra
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let _ = writeln!(
            out,
            "{}:{}:{}: {} {} (fix: {})",
            text(&d["file"]),
            num(&d["line"]),
            num(&d["column"]),
            text(&d["code"]),
            text(&d["message"]),
            text(&d["fix"])
        );
    }
    write_stderr(&out);
}

/// Writes `text` to stderr, ignoring a failed write: an error report on a closed stderr has
/// nowhere to go, and must not panic.
fn write_stderr(text: &str) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// One line per key rdy re-derived on propose or approve (`keys`: block id → new key, K1).
pub(crate) fn new_keys_text(v: &Value) -> String {
    keys_lines(v, " (another block holds the key rdy derived)")
}

/// One `new key <id>: <key><why>` line per entry of `keys` (block id → new key).
pub(crate) fn keys_lines(v: &Value, why: &str) -> String {
    let mut t = String::new();
    for (id, k) in v["keys"].as_object().into_iter().flatten() {
        t.push_str(&format!("\nnew key {id}: {}{why}", text(k)));
    }
    t
}
