//! cc12: `rdy coverage` builds the request for the ctl op of the same name; the renderer turns
//! the reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use crate::output::text;
use rdy_daemon::ErrorReply;
use rdy_schema::CoverageDimension;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct CoverageArgs {
    /// canon (default), a view name, or tag:<version>.
    #[arg(long)]
    scope: Option<String>,
    /// Only the docs at or under this path.
    path: Option<String>,
    /// The budget for the report; issues that do not fit are left out from the end.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
}

/// The text of a `coverage` reply: the score, the dimensions, the lowest pages, the areas and
/// the issues.
fn coverage_text(v: &Value) -> String {
    let mut out = vec![format!(
        "coverage {} ({})",
        v["score"].as_u64().unwrap_or(0),
        text(&v["scope"])
    )];
    let dims: Vec<String> = CoverageDimension::ALL
        .iter()
        .filter_map(|d| {
            v["dimensions"][d.name()]
                .as_u64()
                .map(|s| format!("{} {s}", d.name()))
        })
        .collect();
    if !dims.is_empty() {
        out.push(format!("  {}", dims.join("  ")));
    }
    let pages = v["pages"].as_array().map_or(&[][..], Vec::as_slice);
    if !pages.is_empty() {
        out.push("lowest pages:".to_string());
        for p in pages {
            out.push(format!(
                "  {}  {}",
                p["score"].as_u64().unwrap_or(0),
                text(&p["path"])
            ));
        }
        if let Some(n) = v["pages_omitted"].as_u64().filter(|n| *n > 0) {
            out.push(format!("  … {n} more"));
        }
    }
    let areas = v["areas"].as_array().map_or(&[][..], Vec::as_slice);
    if !areas.is_empty() {
        out.push("areas:".to_string());
        for a in areas {
            out.push(format!(
                "  {}  {}",
                a["score"].as_u64().unwrap_or(0),
                text(&a["area"])
            ));
        }
    }
    let issues = v["issues"].as_array().map_or(&[][..], Vec::as_slice);
    let total = v["issues_total"].as_u64().unwrap_or(0);
    out.push(format!("issues ({total}):"));
    for i in issues {
        let mut line = format!(
            "  {}  {}  {}  {}",
            text(&i["block"]),
            text(&i["dimension"]),
            text(&i["code"]),
            text(&i["fix"])
        );
        if i["dismissed"].is_object() {
            line.push_str(&format!(
                "  (dismissed by {}: {})",
                text(&i["dismissed"]["actor"]),
                text(&i["dismissed"]["reason"])
            ));
        }
        out.push(line);
    }
    let cut = total.saturating_sub(issues.len() as u64);
    if cut > 0 {
        out.push(format!("  … {cut} more (pass --max-tokens for more)"));
    }
    out.join("\n")
}

pub(crate) fn coverage(env: &Env, a: CoverageArgs) -> Result<(Value, String), ErrorReply> {
    let CoverageArgs {
        scope,
        path,
        max_tokens,
    } = a;
    let v = call(
        env,
        "coverage",
        json!({"scope": scope, "path": path, "max_tokens": max_tokens}),
    )?;
    let t = coverage_text(&v);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc12_the_text_lists_dimensions_in_order_and_marks_a_dismissal() {
        let v = json!({
            "scope": "canon", "score": 80,
            "dimensions": {"governed": 100, "grounded": 70},
            "pages": [{"doc": "d-1", "path": "ops", "score": 70}], "pages_omitted": 2,
            "areas": [{"area": "ops", "score": 75}],
            "issues": [{"block": "c-1", "dimension": "consistent", "code": "dismissed_tension",
                "fix": "do it", "dismissed": {"actor": "human:a", "reason": "fine"}}],
            "issues_total": 3
        });
        assert_eq!(
            coverage_text(&v),
            "coverage 80 (canon)\n  grounded 70  governed 100\nlowest pages:\n  70  ops\n  … 2 more\nareas:\n  75  ops\nissues (3):\n  c-1  consistent  dismissed_tension  do it  (dismissed by human:a: fine)\n  … 2 more (pass --max-tokens for more)"
        );
    }
}
