//! ls7: `rdy_site`. The reply is built by `rdy_engine::engine::site_reply`, which the daemon's
//! `site_url` op shares. ec11: `rdy_export`, the print, through `Engine::export`, and pub10:
//! `rdy_render`, the static site, through `Engine::render` (O35: this crate has no edge to the
//! renderer). `rdy_publication_check`: the reference check and disclosure
//! comparison of a publication profile, from `rdy_engine::publication::check`, which the
//! daemon's `publication_check` op shares. `rdy_import`: a writer imports a format-3 tree into a
//! view through `rdy_engine::import_api::run_import`, which the daemon's `import` op shares.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::engine::{ExportRequest, RenderRequest, site_reply};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SiteParams {
    /// canon (default), a version as tag:<version>, a view name or view:<name>, or proposal:P-x@n (P-x follows its latest version). A view or proposal renders in review mode and updates as it changes.
    pub scope: Option<String>,
    /// A publication profile id or key in this scope, or implicit:org. Omit it for all docs.
    pub profile: Option<String>,
    /// Serve canon through this profile on a separate port. Requires profile and scope canon. This changes presentation, not source access.
    pub only: Option<bool>,
}

/// `rdy_site`: the link to the live docs site for `scope`, for the person to open, through
/// `profile` when given (D-CWC91), and on the profile's own port with `only` (D-DK03K). A session
/// is required, as for `rdy_inbox`; every role may inspect, because the site is read-only.
pub fn rdy_site(st: &mut McpState, p: SiteParams) -> Result<Value, RdyError> {
    session(st)?;
    site_reply(
        &*st.engine,
        p.scope.as_deref(),
        p.profile.as_deref(),
        p.only.unwrap_or(false),
    )
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportParams {
    /// The file (pdf, html) or directory (md) to write: absolute, or relative to the project directory. It must not exist; a directory may be empty.
    pub out: String,
    /// pdf, html or md.
    pub format: ExportFormat,
    /// mkdocs: a docs/ folder and mkdocs.yml whose nav follows the folders. techdocs: the same for Backstage TechDocs, with the techdocs-core plugin and a generated-by header in each Markdown and YAML file. Only with md.
    pub layout: Option<ExportLayout>,
    /// canon (the default), a view, tag:<version>, landing:<id> or proposal:P-<id>@<n>. With a profile: canon, tag:<version> or landing:<id>.
    pub scope: Option<String>,
    /// A publication profile id or key in the selected scope, or implicit:org for every doc. Selects whole docs and folders for local output.
    pub profile: Option<String>,
}

/// The print formats, in lower case.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Pdf,
    Html,
    Md,
}

impl ExportFormat {
    fn as_str(self) -> &'static str {
        match self {
            ExportFormat::Pdf => "pdf",
            ExportFormat::Html => "html",
            ExportFormat::Md => "md",
        }
    }
}

/// The Markdown tree's layouts, in lower case.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExportLayout {
    Mkdocs,
    Techdocs,
}

impl ExportLayout {
    fn as_str(self) -> &'static str {
        match self {
            ExportLayout::Mkdocs => "mkdocs",
            ExportLayout::Techdocs => "techdocs",
        }
    }
}

/// `rdy_export`: prints the docs at `out` for a person to read. A session is required, as for
/// `rdy_site`. A relative `out` is under the project directory. A scope reads as every tool reads
/// it: the session's own view needs no record. The daemon prints; an engine with no daemon
/// refuses (O35).
pub fn rdy_export(st: &mut McpState, p: ExportParams) -> Result<Value, RdyError> {
    session(st)?;
    st.engine.export(ExportRequest {
        out: st.engine.root().join(&p.out),
        format: p.format.as_str().to_string(),
        layout: p.layout.map(|l| l.as_str().to_string()),
        scope: p.scope,
        profile: p.profile,
        view: Some(st.view.clone()),
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderParams {
    /// The directory to write: absolute, or relative to the project directory. It must be new, empty, or a site rdy rendered before (replaced as a whole).
    pub out: String,
    /// canon (the default), a view, tag:<version>, landing:<id> or proposal:P-<id>@<n>. With a profile: canon, tag:<version> or landing:<id>.
    pub scope: Option<String>,
    /// A publication profile id or key in the selected scope, or implicit:org for every doc. Selects whole docs and folders for local output.
    pub profile: Option<String>,
}

/// `rdy_render`: builds a static docs site at `out`, the local twin of the live site. A session
/// is required, as for `rdy_export`. A relative `out` is under the project directory. A scope
/// defaults to canon, and a scope reads as every tool reads it. The daemon renders; an engine
/// with no daemon refuses (O35).
pub fn rdy_render(st: &mut McpState, p: RenderParams) -> Result<Value, RdyError> {
    session(st)?;
    st.engine.render(RenderRequest {
        out: st.engine.root().join(&p.out),
        scope: p.scope,
        profile: p.profile,
        view: Some(st.view.clone()),
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicationCheckParams {
    /// A publication profile id or key, or implicit:org for every doc.
    pub profile: String,
    /// canon (default), tag:<name>, landing:<hash>, a view name, view:<name>, or proposal:P-x@n.
    pub scope: Option<String>,
    /// A baseline to compare with: canon, tag:<name> or landing:<hash>. It must hold the same profile id and audience. Omit it for no comparison.
    pub against: Option<String>,
    /// The product version this candidate ships as, such as 1.2.0 or 2.0.0-rc.1 (one leading v is allowed; no +build). Omit it for no release check. Content that is since a later version, and decisions built in one, block the result. Against never supplies it.
    pub release: Option<String>,
}

/// `rdy_publication_check`: the check of a profile, in one read state. A session is required;
/// every role may inspect, because the check reads source and writes nothing. A blocked result
/// is a success: the caller reads the aggregate `status` and relays `warnings`; it authorizes
/// nothing.
pub fn rdy_publication_check(
    st: &mut McpState,
    p: PublicationCheckParams,
) -> Result<Value, RdyError> {
    session(st)?;
    // the report is size-checked: a build warning it will carry stays inside the limit
    let reserve = st
        .build_warning_due
        .as_ref()
        .map_or(0, |w| w.to_string().len() + r#","warnings":[]"#.len());
    rdy_engine::publication::check_reserving(
        &st.engine.read(),
        &p.profile,
        p.scope.as_deref(),
        p.against.as_deref(),
        p.release.as_deref(),
        reserve,
    )
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportParams {
    /// The import directory, absolute or relative to the project directory.
    pub dir: String,
    /// The view to import into. Import never writes canon.
    pub view: String,
    /// Read dir as a Markdown source, not a format-3 tree. Default false.
    #[serde(default)]
    pub md: Option<bool>,
    /// Preview the Markdown import and write nothing. Needs md: true. Default false.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// The source_digest of a preview. The import refuses when the source changed. Needs md: true.
    #[serde(default)]
    pub expected_source: Option<String>,
}

/// `rdy_import`: imports the format-3 tree at `dir` into `view` as this session (D-Y9DNZ), or with
/// `md` the Markdown source at `dir` (D-19BZD): `dry_run` previews it and writes nothing, and
/// `expected_source` is the digest a preview gave. The session is resolved first, then the path
/// (a relative `dir` is under the project directory), then the engine imports under the ordinary
/// write rule: a reader is refused, canon is never written, and the reply is the import reply the
/// command line shares. The session is never widened: an agent stays an agent, and a person
/// approves imported statuses at landing.
pub fn rdy_import(st: &mut McpState, p: ImportParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.strip_prefix("view:").unwrap_or(&p.view).to_string();
    let dir = st.engine.root().join(&p.dir);
    let options = rdy_engine::import_api::ImportOptions {
        md: p.md.unwrap_or(false),
        dry_run: p.dry_run.unwrap_or(false),
        expected_source: p.expected_source,
    };
    rdy_engine::import_api::run_import_with(&*st.engine, &s, &view, &dir, &options)
}
