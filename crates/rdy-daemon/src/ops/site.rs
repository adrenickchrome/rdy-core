//! The site ops: render, export (a print, D-EVRNM) and import.
use super::*;
use rdy_engine::consequence::write_and_report_as;
use rdy_engine::{RdyError, ReadState, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutArgs {
    out: String,
    scope: Option<String>,
    profile: Option<String>,
}

/// The print arguments of the `export` op (O1): `format` is `pdf`, `html` or `md`; `layout` is
/// `mkdocs`, with `md` only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportArgs {
    out: String,
    format: String,
    layout: Option<String>,
    scope: Option<String>,
    profile: Option<String>,
}

/// The shape of the `export` op's arguments.
pub(super) const EXPORT_SHAPE: &str = "{out, format, layout?, scope?, profile?}";

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FormatArg {
    Pdf,
    Html,
    Md,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum LayoutArg {
    Mkdocs,
}

/// `text` as one of the closed set `T` names; anything else is the op's argument error.
fn closed<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, RdyError> {
    use serde::de::IntoDeserializer;
    T::deserialize(IntoDeserializer::<serde::de::value::Error>::into_deserializer(text)).map_err(
        |e| {
            RdyError::new(
                rdy_engine::E_BAD_INPUT,
                format!("export: {e}"),
                format!("args for export: {EXPORT_SHAPE}"),
            )
        },
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportArgs {
    dir: String,
    view: String,
}

pub(super) fn render(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    render_static(
        &st,
        a.scope.as_deref().unwrap_or("canon"),
        Some(&cx.session.default_view()),
        &resolve_path(cx.cwd, &a.out),
        a.profile.as_deref(),
    )
}

pub(super) fn export(cx: &OpContext, a: ExportArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    export_print(
        &st,
        a.scope.as_deref().unwrap_or("canon"),
        Some(&cx.session.default_view()),
        &resolve_path(cx.cwd, &a.out),
        &a.format,
        a.layout.as_deref(),
        a.profile.as_deref(),
    )
}

/// Renders a static site at `out` and replies with the render summary. `scope_text` is `canon`
/// when the caller named none. Without `profile` it is the full source of the scope, as it has
/// always been (RULING F2): `own_view` is the asking session's view, which a scope may name
/// before it has a record. With `profile` (an id, a key, or `implicit:org`) the text goes
/// unchanged to the profile's own entry point, which resolves it once in the same state: canon,
/// a tag or a landing only, and a profile that cannot be resolved never falls back to the
/// source. The `render` op and the daemon's `Engine::render` (`rdy_render`) both render through it.
pub(crate) fn render_static(
    st: &ReadState,
    scope_text: &str,
    own_view: Option<&str>,
    out: &Path,
    profile: Option<&str>,
) -> Result<Value, RdyError> {
    let summary = match profile {
        Some(p) => rdy_render::render_profile_site(st, scope_text, p, out)?,
        None => {
            let scope = st.resolve_read_scope(scope_text, own_view)?;
            rdy_render::render_site(st, &scope, out)?
        }
    };
    Ok(to_json(&summary))
}

/// Prints `scope_text` at `out` as `format` (`pdf`, `html` or `md`) and `layout` (`mkdocs`), and
/// replies with the print's summary. The scope and the profile read as [`render_static`] reads
/// them: with a profile, its selected view and its reference check; without, the full source.
/// The `export` op and the daemon's `Engine::export` (`rdy_export`) both print through it.
pub(crate) fn export_print(
    st: &ReadState,
    scope_text: &str,
    own_view: Option<&str>,
    out: &Path,
    format: &str,
    layout: Option<&str>,
    profile: Option<&str>,
) -> Result<Value, RdyError> {
    use rdy_render::print::{Format, Layout};
    // a scope that does not resolve is refused before a format that does not parse, as it was
    let scope = match profile {
        Some(_) => None,
        None => Some(st.resolve_read_scope(scope_text, own_view)?),
    };
    let format = match closed::<FormatArg>(format)? {
        FormatArg::Pdf => Format::Pdf,
        FormatArg::Html => Format::Html,
        FormatArg::Md => Format::Md,
    };
    let layout = layout
        .map(closed::<LayoutArg>)
        .transpose()?
        .map(|LayoutArg::Mkdocs| Layout::Mkdocs);
    let summary = match (profile, scope) {
        (Some(p), _) => rdy_render::print::export_profile(st, scope_text, p, out, format, layout)?,
        (None, Some(scope)) => rdy_render::print::export(st, &scope, out, format, layout)?,
        (None, None) => unreachable!("a scope is resolved whenever no profile is given"),
    };
    Ok(to_json(&summary))
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
