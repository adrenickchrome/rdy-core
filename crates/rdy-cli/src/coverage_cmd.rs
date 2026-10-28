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
    /// Only the blocks at or under this path. A block in a doc takes the doc's path; a block in a folder takes the folder's.
    path: Option<String>,
    /// The budget for the report; issues that do not fit are left out from the end.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
}

/// The onboarding line of a reply that carries `onboarding` (FW-CO4a): the score against the
/// target, and the next phase with its open issues per dimension, or that onboarding is done.
/// `corpus` labels the line `corpus coverage` when the report above it covers only a path, so the
/// two numbers do not carry one label.
pub(crate) fn onboarding_line(v: &Value, corpus: bool) -> Option<String> {
    let o = v["onboarding"].as_object()?;
    let score = o.get("score")?.as_u64()?;
    let target = o.get("target")?.as_u64()?;
    let label = if corpus {
        "corpus coverage"
    } else {
        "coverage"
    };
    let head = format!("{label} {score} of a target {target}");
    let Some(next) = o.get("next").and_then(Value::as_str) else {
        return Some(format!("{head}; onboarding is done"));
    };
    let phase = o
        .get("phases")
        .and_then(Value::as_array)
        .and_then(|ps| ps.iter().find(|p| p["phase"] == next));
    let counts = phase.map_or(Vec::new(), |p| {
        let by = &p["dimension_issues"];
        let named: Vec<String> = CoverageDimension::ALL
            .iter()
            .filter_map(|d| by[d.name()].as_u64().map(|n| format!("{} {n}", d.name())))
            .collect();
        if named.is_empty() {
            // no open issue: the phase still falls short, so name its dimensions
            let dims = p["dimensions"].as_array().map_or(&[][..], Vec::as_slice);
            return dims
                .iter()
                .map(|d| format!("{} 0", text(d)))
                .collect::<Vec<_>>();
        }
        named
    });
    Some(format!(
        "{head}; next phase: {next} ({})",
        counts.join(", ")
    ))
}

/// The text of a `coverage` reply: the score, the dimensions, the lowest pages, the areas and
/// the issues.
fn coverage_text(v: &Value, narrowed: bool) -> String {
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
    out.extend(onboarding_line(v, narrowed));
    out.join("\n")
}

pub(crate) fn coverage(env: &Env, a: CoverageArgs) -> Result<(Value, String), ErrorReply> {
    let CoverageArgs {
        scope,
        path,
        max_tokens,
    } = a;
    let narrowed = path.is_some();
    let v = call(
        env,
        "coverage",
        json!({"scope": scope, "path": path, "max_tokens": max_tokens}),
    )?;
    let t = coverage_text(&v, narrowed);
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
            coverage_text(&v, false),
            "coverage 80 (canon)\n  grounded 70  governed 100\nlowest pages:\n  70  ops\n  … 2 more\nareas:\n  75  ops\nissues (3):\n  c-1  consistent  dismissed_tension  do it  (dismissed by human:a: fine)\n  … 2 more (pass --max-tokens for more)"
        );
    }

    #[test]
    fn cc17_the_onboarding_line_names_the_next_phase_or_says_done() {
        let o = json!({"onboarding": {"target": 80, "score": 66, "done": false, "next": "consistency",
            "phases": [{"phase": "consistency", "dimensions": ["consistent", "current"],
                "dimension_issues": {"consistent": 2, "current": 1}}]}});
        assert_eq!(
            onboarding_line(&o, false).unwrap(),
            "coverage 66 of a target 80; next phase: consistency (consistent 2, current 1)"
        );
        let done = json!({"onboarding": {"target": 80, "score": 91, "done": true, "next": null, "phases": []}});
        assert_eq!(
            onboarding_line(&done, false).unwrap(),
            "coverage 91 of a target 80; onboarding is done"
        );
        assert_eq!(onboarding_line(&json!({}), false), None);
    }

    /// cc17 fix 2: with a path the report is the path's, the onboarding line is the corpus's.
    #[test]
    fn cc17_a_path_report_labels_the_onboarding_line_as_the_corpus() {
        let v = json!({"scope": "canon", "score": 60, "issues_total": 0,
            "onboarding": {"target": 80, "score": 75, "done": false, "next": "entities",
                "phases": [{"phase": "entities", "dimensions": ["grounded"], "dimension_issues": {"grounded": 3}}]}});
        let narrowed = coverage_text(&v, true);
        assert_eq!(narrowed.lines().next().unwrap(), "coverage 60 (canon)");
        assert_eq!(
            narrowed.lines().last().unwrap(),
            "corpus coverage 75 of a target 80; next phase: entities (grounded 3)"
        );
        let whole = coverage_text(&v, false);
        assert_eq!(
            whole.lines().last().unwrap(),
            "coverage 75 of a target 80; next phase: entities (grounded 3)"
        );
    }
}
