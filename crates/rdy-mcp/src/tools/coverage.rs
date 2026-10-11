//! `rdy_coverage`: the coverage report of a scope or a path (FW-CV1, D-KE84J). The daemon's
//! `coverage` op builds its reply with [`coverage_reply`], so both doors cut by the same budget.
use super::*;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageParams {
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// Only the docs at or under this path (a doc, a folder or a block's path).
    pub path: Option<String>,
    /// Cut the answer to about this many tokens (the issues list is shortened).
    pub max_tokens: Option<usize>,
}

/// The report of `scope` (narrowed to `path`) as JSON, cut to `max_tokens`: the longest list is
/// shortened (the issues only), and the reply says `truncated` and `omitted`.
pub fn coverage_reply(
    st: &ReadState,
    scope: &Scope,
    path: Option<&str>,
    max_tokens: Option<usize>,
) -> Result<Value, RdyError> {
    let report = st.coverage_report(scope, path)?;
    let v = serde_json::to_value(report).expect("coverage report json");
    Ok(cut_issues(v, max_tokens))
}

/// Fits `v` into `max_tokens` (default 4000) by shortening only the `issues` list; the pages,
/// the areas and the kinds stay whole, so `truncated` and `omitted` always speak of issues.
fn cut_issues(mut v: Value, max_tokens: Option<usize>) -> Value {
    let max = max_tokens.unwrap_or(4000);
    if tokens_of(&v) <= max {
        return v;
    }
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let full = obj
        .get("issues")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let fits = |obj: &mut serde_json::Map<String, Value>, n: usize| {
        obj.insert("issues".into(), Value::Array(full[..n].to_vec()));
        obj.insert("truncated".into(), json!(true));
        obj.insert("omitted".into(), json!(full.len() - n));
        tokens_of(&Value::Object(obj.clone())) <= max
    };
    let (mut lo, mut hi) = (0usize, full.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(obj, mid) {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    fits(obj, lo);
    v
}

/// `rdy_coverage`: the report of the scope (default the session's view) or a path.
pub fn rdy_coverage(st: &mut McpState, p: CoverageParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    coverage_reply(&st_, &scope, p.path.as_deref(), p.max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cc12_the_budget_cuts_issues_even_when_pages_are_longer() {
        let pages: Vec<Value> = (0..20)
            .map(|i| json!({"doc": format!("d-{i}"), "path": format!("a/long/path/{i}"), "score": 50}))
            .collect();
        let issues: Vec<Value> = (0..10)
            .map(|i| json!({"block": format!("c-{i}"), "code": "x", "fix": "f"}))
            .collect();
        let v = json!({"pages": pages, "issues": issues, "issues_total": 10});
        let out = cut_issues(v, Some(300));
        assert_eq!(out["pages"].as_array().unwrap().len(), 20, "{out}");
        assert_eq!(out["truncated"], true, "{out}");
        let kept = out["issues"].as_array().unwrap().len() as u64;
        assert!(kept < 10, "{out}");
        assert_eq!(kept + out["omitted"].as_u64().unwrap(), 10, "{out}");
    }
}
