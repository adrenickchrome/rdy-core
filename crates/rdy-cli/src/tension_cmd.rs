//! mten1/cten1: `rdy tensions` and `rdy resolve`. Each builds the request for the ctl op of the
//! same name; the renderer turns the reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use crate::output::{hash_short, num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct TensionsArgs {
    /// inflight (default: every tension), canon (tensions whose sides are all canon) or a view.
    #[arg(long)]
    scope: Option<String>,
    /// Only this kind, e.g. premise_shift.
    #[arg(long)]
    kind: Option<String>,
    /// Only this severity: blocking or advisory.
    #[arg(long)]
    severity: Option<String>,
    /// open (default) or dismissed.
    #[arg(long)]
    status: Option<String>,
    /// Only tensions with a change side in this view.
    #[arg(long)]
    view: Option<String>,
    /// Only tensions that involve a block in this pack area.
    #[arg(long)]
    area: Option<String>,
    /// Only tensions that involve this block (id, key or path) or this change (hex prefix).
    #[arg(long)]
    involving: Option<String>,
    /// Also list the dismissal reviews.
    #[arg(long)]
    reviews: bool,
    /// The budget for the listing; tensions that do not fit are left out from the end.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
}

#[derive(clap::Args)]
pub(crate) struct ResolveArgs {
    /// The tension: tn-<hex>, or a unique prefix of at least 6 hex digits.
    tension: Option<String>,
    /// A passing mention instead of a tension: the block and the entity it names.
    #[arg(long, num_args = 2, value_names = ["BLOCK", "ENTITY"], conflicts_with = "tension")]
    mention: Option<Vec<String>>,
    /// Dismiss the tension or the mention, and why.
    #[arg(long, conflicts_with = "restore")]
    dismiss: Option<String>,
    /// Restore a dismissed mention, and why.
    #[arg(long)]
    restore: Option<String>,
    /// Where the mention is checked (default: your view), e.g. view:work.
    #[arg(long, requires = "mention")]
    scope: Option<String>,
}

/// `<id>  <severity>  <kind>  <status>  <summary>`, then the sides, the proposal of a landing
/// form, and the dismissal.
fn tension_text(t: &Value) -> String {
    let mut out = format!(
        "{}  {}  {}  {}  {}",
        text(&t["id"]),
        text(&t["severity"]),
        text(&t["kind"]),
        text(&t["status"]),
        text(&t["summary"])
    );
    let sides: Vec<String> = t["sides"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let label = text(&s["side"]);
            let label = if s["change"].is_string() {
                hash_short(label)
            } else {
                label
            };
            let mut one = label.to_string();
            if let Some(a) = s["actor"].as_str() {
                one.push_str(&format!(" ({a})"));
            }
            one
        })
        .collect();
    if !sides.is_empty() {
        out.push_str(&format!("\n  sides: {}", sides.join("; ")));
    }
    if let Some(p) = t["proposal"].as_str() {
        out.push_str(&format!(
            "\n  landing form of {p} (not in flight; the gate judges it)"
        ));
    }
    if t["dismissal"].is_object() {
        out.push_str(&format!(
            "\n  dismissed by {}: {}",
            text(&t["dismissal"]["actor"]),
            text(&t["dismissal"]["reason"])
        ));
    }
    out
}

/// `<review 12>  <tension 12>  <kind>  <actor>  holds|lapsed: <reason>`.
fn review_text(r: &Value) -> String {
    format!(
        "review {}  {}  {}  {}  {}: {}",
        hash_short(text(&r["review"])),
        hash_short(text(&r["tension"])),
        text(&r["kind"]),
        text(&r["actor"]),
        if r["holds"].as_bool() == Some(true) {
            "holds"
        } else {
            "lapsed"
        },
        text(&r["reason"])
    )
}

/// The text of a `tensions` reply: one entry per tension, the reviews, then the counts.
fn tensions_render(v: &Value) -> String {
    let rows = v["tensions"].as_array().map_or(&[][..], Vec::as_slice);
    let omitted = v["omitted"].as_u64().unwrap_or(0);
    let mut lines: Vec<String> = if rows.is_empty() && omitted == 0 {
        vec!["no tensions".to_string()]
    } else {
        rows.iter().map(tension_text).collect()
    };
    if omitted > 0 {
        lines.push(format!(
            "{omitted} more not listed (pass --max-tokens for more)"
        ));
    }
    for r in v["reviews"].as_array().into_iter().flatten() {
        lines.push(review_text(r));
    }
    if let Some(n) = v["reviews_omitted"].as_u64().filter(|n| *n > 0) {
        lines.push(format!(
            "{n} reviews not listed (pass --max-tokens for more)"
        ));
    }
    let c = &v["counts"];
    lines.push(format!(
        "{} open ({} blocking), {} dismissed",
        num(&c["open"]),
        num(&c["blocking_open"]),
        num(&c["dismissed"])
    ));
    lines.join("\n")
}

pub(crate) fn tensions(env: &Env, a: TensionsArgs) -> Result<(Value, String), ErrorReply> {
    let TensionsArgs {
        scope,
        kind,
        severity,
        status,
        view,
        area,
        involving,
        reviews,
        max_tokens,
    } = a;
    let v = call(
        env,
        "tensions",
        json!({
            "scope": scope,
            "kind": kind,
            "severity": severity,
            "status": status,
            "view": view,
            "area": area,
            "involving": involving,
            "reviews": reviews.then_some(true),
            "max_tokens": max_tokens,
        }),
    )?;
    let t = tensions_render(&v);
    Ok((v, t))
}

pub(crate) fn resolve(env: &Env, a: ResolveArgs) -> Result<(Value, String), ErrorReply> {
    let ResolveArgs {
        tension,
        mention,
        dismiss,
        restore,
        scope,
    } = a;
    let mention = mention.map(|m| json!({"block": m[0], "entity": m[1]}));
    let reason = |r: Option<String>| r.map(|r| json!({"reason": r}));
    let v = call(
        env,
        "resolve",
        json!({
            "tension": tension,
            "mention": mention,
            "dismiss": reason(dismiss),
            "restore": reason(restore),
            "scope": scope,
        }),
    )?;
    let t = if v["entity"].is_string() {
        let did = if text(&v["verdict"]) == "restore" {
            "restored"
        } else {
            "dismissed"
        };
        format!(
            "{did} the mention of {} in {} (review {})",
            text(&v["entity"]),
            text(&v["block"]),
            hash_short(text(&v["review"]))
        )
    } else {
        format!(
            "dismissed {} (review {})",
            text(&v["id"]),
            hash_short(text(&v["review"]))
        )
    };
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cten1_a_landing_form_names_its_proposal() {
        let t = json!({"id": "tn-0123456789ab", "severity": "blocking", "kind": "duplicate_definition",
            "status": "open", "summary": "t-A and t-B both define \"fee\"", "sides": [],
            "proposal": "P-abcdef012345@1"});
        assert_eq!(
            tension_text(&t),
            "tn-0123456789ab  blocking  duplicate_definition  open  t-A and t-B both define \"fee\"\n  landing form of P-abcdef012345@1 (not in flight; the gate judges it)"
        );
    }
}
