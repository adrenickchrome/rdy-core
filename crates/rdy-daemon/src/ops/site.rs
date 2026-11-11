//! The site ops: render, export (a print, D-EVRNM) and import.
use super::*;
use rdy_engine::{RdyError, ReadState};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutArgs {
    out: String,
    scope: Option<String>,
    profile: Option<String>,
}

/// The print arguments of the `export` op (O1): `format` is `pdf`, `html` or `md`; `layout` is
/// `mkdocs` or `techdocs`, with `md` only.
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
    Techdocs,
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
    #[serde(default)]
    md: Option<bool>,
    #[serde(default)]
    dry_run: Option<bool>,
    #[serde(default)]
    expected_source: Option<String>,
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

/// Prints `scope_text` at `out` as `format` (`pdf`, `html` or `md`) and `layout` (`mkdocs` or `techdocs`), and
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
        .map(|l| match l {
            LayoutArg::Mkdocs => Layout::Mkdocs,
            LayoutArg::Techdocs => Layout::Techdocs,
        });
    let summary = match (profile, scope) {
        (Some(p), _) => rdy_render::print::export_profile(st, scope_text, p, out, format, layout)?,
        (None, Some(scope)) => rdy_render::print::export(st, &scope, out, format, layout)?,
        (None, None) => unreachable!("a scope is resolved whenever no profile is given"),
    };
    Ok(to_json(&summary))
}

/// The import op: the engine's import reply (`run_import_with`, which the chat tool shares) for the
/// session as it is, with the Markdown options `md`, `dry_run` and `expected_source`. A writer, a reviewer and a person's approver import under the role rule of
/// every write (D-Y9DNZ); the engine refuses a reader and canon.
pub(super) fn import(cx: &OpContext, a: ImportArgs) -> Result<Value, RdyError> {
    let view = view_of(cx.session, Some(a.view));
    let dir = resolve_path(cx.cwd, &a.dir);
    let options = rdy_engine::import_api::ImportOptions {
        md: a.md.unwrap_or(false),
        dry_run: a.dry_run.unwrap_or(false),
        expected_source: a.expected_source,
    };
    rdy_engine::import_api::run_import_with(cx.engine, cx.session, &view, &dir, &options)
}
