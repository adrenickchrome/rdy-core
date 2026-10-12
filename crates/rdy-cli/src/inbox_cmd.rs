//! ib9: `rdy inbox [--ack]`, the CLI twin of `rdy_inbox` (D-TPZ0W, FW-UX1). The op reads the
//! inbox of the session that runs it: the human's (run `cli`) or, with `RDY_ACTOR`, an agent's.
use crate::conn::{Env, call};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct InboxArgs {
    /// Move your cursor past the entries shown.
    #[arg(long)]
    ack: bool,
}

pub(crate) fn inbox(env: &Env, a: InboxArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "inbox", json!({"ack": a.ack}))?;
    let text = render(&v);
    Ok((v, text))
}

/// The reply as text: one `{event}  {line}` per entry, then the cursor, what to do when more
/// entries wait, and `acked`. A fresh inbox says only that it started.
fn render(v: &Value) -> String {
    if v["started"] == true {
        return "inbox started: events from now on show here".to_string();
    }
    let mut lines: Vec<String> = v["entries"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|e| {
            format!(
                "{}  {}",
                e["event"].as_str().unwrap_or(""),
                e["line"].as_str().unwrap_or("")
            )
        })
        .collect();
    lines.push(format!("cursor: {}", v["cursor"].as_str().unwrap_or("")));
    if v["more"] == true {
        lines.push("more entries: run rdy inbox --ack, then rdy inbox again".to_string());
    }
    if v["acked"] == true {
        lines.push("acked".to_string());
    }
    lines.join("\n")
}
