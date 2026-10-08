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
    tension: String,
    /// Dismiss the tension, and why.
    #[arg(long)]
    dismiss: String,
}

/// `<id>  <severity>  <kind>  <status>  <summary>`, then the sides and the dismissal.
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
    let mut lines: Vec<String> = if rows.is_empty() {
        vec!["no tensions".to_string()]
    } else {
        rows.iter().map(tension_text).collect()
    };
    if let Some(n) = v["omitted"].as_u64() {
        lines.push(format!("{n} more not listed (pass --max-tokens for more)"));
    }
    for r in v["reviews"].as_array().into_iter().flatten() {
        lines.push(review_text(r));
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
    let ResolveArgs { tension, dismiss } = a;
    let v = call(
        env,
        "resolve",
        json!({"tension": tension, "dismiss": {"reason": dismiss}}),
    )?;
    let t = format!(
        "dismissed {} (review {})",
        text(&v["id"]),
        hash_short(text(&v["review"]))
    );
    Ok((v, t))
}
