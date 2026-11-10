//! The import reply (D-KQ4CA, chat first): `rdy import` through any transport. The import is a
//! write ([`crate::consequence::write_and_report_as`]) read back as the importing session, never
//! as a source author; then each warning names its doc, and the reply carries the import's
//! coverage baseline and onboarding path (D-C6HHV, D-F2SRE). The import is committed before the
//! baseline is read, so a failed baseline is `coverage_error` on a reply that stands: a client
//! must never import again.
use crate::consequence::write_and_report_as;
use crate::coverage::{CoverageReport, Onboarding, coverage_error_text};
use crate::engine::Engine;
use crate::{RdyError, Scope, Session};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Imports the export tree at `dir` (a path already resolved) into `view` as `session`, and
/// replies with the write report, `warning_docs`, and `coverage` and `onboarding` (or
/// `coverage_error`). The write receipt stays under `briefing` (D-B2KRN).
pub fn run_import(
    engine: &dyn Engine,
    session: &Session,
    view: &str,
    dir: &Path,
) -> Result<Value, RdyError> {
    let scope = Scope::View(view.to_string());
    let (s, target, tree) = (session.clone(), view.to_string(), dir.to_path_buf());
    // the reply is the importer's: every author change is read as this session
    let mut v = write_and_report_as(engine, session, view, None, move |r| {
        r.import(&s, &target, &tree)
    })?;
    // each warning names its doc: the title as imported, then the id (the CLI prints it)
    let st = engine.read();
    let docs: Map<String, Value> = v["warnings"]
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
    // the import's baseline, page by page, and the path to the target: the import is committed,
    // so a failure here must not make the reply an error
    let report = crate::import_prepared::fault(crate::import_prepared::ImportFault::Baseline)
        .and_then(|()| st.coverage_report(&scope, None));
    attach_baseline(&mut v, report, st.onboarding(&scope));
    Ok(v)
}

/// Sets `coverage` and `onboarding` on the import reply. Set here, so it replaces any `coverage`
/// the write reply carries, and any `coverage_error` of that coverage change. When either read
/// fails, both keys are left out, `coverage_error` says why, and the reply stands.
fn attach_baseline(
    v: &mut Value,
    report: Result<CoverageReport, RdyError>,
    onboarding: Result<Onboarding, RdyError>,
) {
    match (report, onboarding) {
        (Ok(report), Ok(onboarding)) => {
            v["coverage"] = json!({
                "score": report.score,
                "dimensions": report.dimensions,
                "pages": report.pages,
            });
            v["onboarding"] = serde_json::to_value(&onboarding).expect("a reply serializes");
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
                    Value::String(coverage_error_text(&e)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ui2: the import is committed before the baseline is read, so a failure there leaves the
    /// reply whole with `coverage_error` (a client never imports again).
    #[test]
    fn ui2_a_failed_baseline_keeps_the_import_reply() {
        let mut v = json!({"changes": ["a", "b"], "coverage": {"before": 1, "after": 2}});
        let boom = || RdyError::new("E_INTERNAL", "boom", "retry");
        attach_baseline(&mut v, Err(boom()), Err(boom()));
        assert_eq!(v["changes"], json!(["a", "b"]), "the import reply stays");
        assert!(v.get("coverage").is_none(), "{v}");
        assert!(v.get("onboarding").is_none(), "{v}");
        assert_eq!(v["coverage_error"], "E_INTERNAL: boom", "{v}");
    }

    /// A good baseline replaces the write reply's coverage change and its error.
    #[test]
    fn ui2_a_good_baseline_clears_the_coverage_change_error() {
        let dir = tempfile::tempdir().unwrap();
        let r = crate::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let st = r.read();
        let mut v = json!({"changes": ["a"], "coverage_error": "E_IO: tensions busy"});
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
