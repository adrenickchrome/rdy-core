//! The site ops: render, export and import.
use super::*;
use rdy_engine::consequence::write_and_report_as;
use rdy_engine::{RdyError, Scope};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutArgs {
    out: String,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportArgs {
    dir: String,
    view: String,
}

pub(super) fn render(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let s = rdy_render::render_site(&st, &scope, &resolve_path(cx.cwd, &a.out))?;
    Ok(to_json(&s))
}

pub(super) fn export(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.export(&scope, &resolve_path(cx.cwd, &a.out))?))
}

/// Sets `coverage` and `onboarding` on the import reply. Set here, so it replaces any `coverage`
/// the write reply carries, and any `coverage_error` of that coverage change. When either read
/// fails, both keys are left out, `coverage_error` says why, and the reply stands.
fn attach_baseline(
    v: &mut Value,
    report: Result<rdy_engine::coverage::CoverageReport, RdyError>,
    onboarding: Result<rdy_engine::coverage::Onboarding, RdyError>,
) {
    match (report, onboarding) {
        (Ok(report), Ok(onboarding)) => {
            v["coverage"] = json!({
                "score": report.score,
                "dimensions": report.dimensions,
                "pages": report.pages,
            });
            v["onboarding"] = to_json(&onboarding);
            // the baseline replaces the write reply's coverage change, and its error with it
            if let Some(o) = v.as_object_mut() {
                o.remove("coverage_error");
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            if let Some(o) = v.as_object_mut() {
                o.remove("coverage");
                o.remove("onboarding");
                o.insert(
                    "coverage_error".into(),
                    Value::String(rdy_engine::coverage::coverage_error_text(&e)),
                );
            }
        }
    }
}

pub(super) fn import(cx: &OpContext, a: ImportArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    let view = view_of(&s, Some(a.view));
    let dir = resolve_path(cx.cwd, &a.dir);
    let scope = Scope::View(view.clone());
    let target = view.clone();
    // the reply is the importer's: every author change is read as this session (finding 11)
    let reader = s.clone();
    let mut v = write_and_report_as(cx.engine, &reader, &target, None, move |r| {
        r.import(&s, &view, &dir)
    })?;
    // each warning names its doc: the title as imported, then the id (the CLI prints it)
    let st = cx.engine.read();
    let docs: serde_json::Map<String, Value> = v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["block"].as_str())
        .filter_map(|id| {
            let bid = rdy_model::BlockId::parse(id).ok()?;
            let label = match st.block(&scope, &bid).ok().and_then(|b| {
                b.field_str("title")
                    .or(b.field_str("name"))
                    .map(str::to_string)
            }) {
                Some(t) if !t.trim().is_empty() => format!("{} ({id}): ", t.trim()),
                _ => format!("{id}: "),
            };
            Some((id.to_string(), Value::String(label)))
        })
        .collect();
    if !docs.is_empty() {
        v["warning_docs"] = Value::Object(docs);
    }
    // the import's baseline, page by page (D-C6HHV), and the path to the target (D-F2SRE). The
    // import is committed, so a failure here must not make the reply an error.
    attach_baseline(
        &mut v,
        st.coverage_report(&scope, None),
        st.onboarding(&scope),
    );
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cc17 fix 1: the import is committed before the baseline is read, so a failure there must
    /// not turn the reply into an error (an agent would import again).
    #[test]
    fn cc17_a_failed_baseline_leaves_the_import_reply_whole() {
        let mut v = json!({"changes": 2, "coverage": {"before": 1, "after": 2}});
        let boom = || RdyError::new("E_INTERNAL", "boom", "retry");
        attach_baseline(&mut v, Err(boom()), Err(boom()));
        assert_eq!(v["changes"], 2, "the import reply stays");
        assert!(v.get("coverage").is_none(), "{v}");
        assert!(v.get("onboarding").is_none(), "{v}");
        assert_eq!(v["coverage_error"], "E_INTERNAL: boom", "{v}");
    }

    /// Final review M5: a write reply whose coverage change failed carries `coverage_error`; a
    /// good baseline replaces that coverage, so the error goes with it.
    #[test]
    fn cc17_a_good_baseline_clears_the_coverage_change_error() {
        let dir = tempfile::tempdir().unwrap();
        let r = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let st = r.read();
        let mut v = json!({"changes": 2, "coverage_error": "E_IO: tensions busy"});
        attach_baseline(
            &mut v,
            st.coverage_report(&Scope::Canon, None),
            st.onboarding(&Scope::Canon),
        );
        assert!(v.get("coverage").is_some(), "{v}");
        assert!(v.get("onboarding").is_some(), "{v}");
        assert!(v.get("coverage_error").is_none(), "{v}");
    }
}
