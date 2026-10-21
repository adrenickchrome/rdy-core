use crate::brief_text;
use crate::conn::{Env, call, path_arg};
use crate::output::{num, text, warning_lines};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args)]
pub(crate) struct RenderArgs {
    /// Output directory: new, empty, or a site rdy rendered before (replaced as a whole).
    #[arg(long)]
    out: PathBuf,
    /// canon, an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
    /// A publication profile id or key in the selected scope, or implicit:org for every doc. Selects whole docs and folders for local output.
    #[arg(long)]
    profile: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ExportArgs {
    /// The file (pdf, html) or directory (md) to write. It must not exist; a directory may be empty.
    #[arg(long)]
    out: PathBuf,
    /// pdf, html or md.
    #[arg(long, value_parser = ["pdf", "html", "md"])]
    format: String,
    /// mkdocs: a docs/ folder and mkdocs.yml whose nav follows the folders. techdocs: the same for Backstage TechDocs, with the techdocs-core plugin and a generated-by header in each Markdown and YAML file. Only with --format md.
    #[arg(long, value_parser = ["mkdocs", "techdocs"])]
    layout: Option<String>,
    /// canon, an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
    /// A publication profile id or key in the selected scope, or implicit:org for every doc. Selects whole docs and folders for local output.
    #[arg(long)]
    profile: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ImportArgs {
    dir: PathBuf,
    #[arg(long)]
    view: String,
}

#[derive(clap::Args)]
pub(crate) struct SiteArgs {
    /// canon (default), a version as tag:<version>, a view name or view:<name>, or proposal:P-x@n. A view or proposal opens in review mode.
    #[arg(long)]
    scope: Option<String>,
    // the help is given in full: a doc comment would lose its closing period
    #[arg(
        long,
        help = "Show this publication profile's selection. Accepts an id, a key, or implicit:org."
    )]
    profile: Option<String>,
}

/// `rdy site`: the link to the live docs site, the CLI twin of `rdy_site`. The URL alone on one
/// line, then `changes: <url>` when the scope has a diff summary, then the LAN URL when the site is
/// open to the network, then the render's warnings for the scope's pages as `rdy render` prints them. The LAN warning and its fix go to stderr.
/// With `--profile`, the link is to that profile's selection of the scope (D-CWC91); `--json`
/// prints the `site_url` reply as it is.
pub(crate) fn site(env: &Env, a: SiteArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(
        env,
        "site_url",
        json!({"scope": a.scope, "profile": a.profile}),
    )?;
    let mut lines = vec![text(&v["url"]).to_string()];
    if !v["changes"].is_null() {
        lines.push(format!("changes: {}", text(&v["changes"])));
    }
    if !v["lan_url"].is_null() {
        lines.push(text(&v["lan_url"]).to_string());
    }
    lan_warning(&v);
    let t = format!("{}{}", lines.join("\n"), warning_lines(&v["warnings"]));
    Ok((v, t))
}

/// The structured LAN warning of a `site_url` reply, if any, on stderr: `warning: <message>`,
/// then `fix: <fix>`, one line each, in text and JSON mode alike.
pub(crate) fn lan_warning(v: &Value) {
    if !v["warning"].is_null() {
        eprintln!(
            "warning: {}\nfix: {}",
            text(&v["warning"]["message"]),
            text(&v["warning"]["fix"])
        );
    }
}

pub(crate) fn render(env: &Env, a: RenderArgs) -> Result<(Value, String), ErrorReply> {
    let RenderArgs {
        out,
        scope,
        profile,
    } = a;
    let v = call(
        env,
        "render",
        json!({"out": path_arg(&out)?, "scope": scope, "profile": profile}),
    )?;
    let t = format!(
        "rendered {} pages, {} permalinks and {} assets to {}{}",
        num(&v["pages"]),
        num(&v["redirects"]),
        num(&v["assets"]),
        out.display(),
        warning_lines(&v["warnings"])
    );
    Ok((v, t))
}

/// `rdy export`: prints the docs, the CLI twin of `rdy_export` (D-EVRNM, O1). One line names the
/// docs, folders, format, path and bytes, then the pages of a PDF; then the warnings as `rdy
/// render` prints them.
pub(crate) fn export(env: &Env, a: ExportArgs) -> Result<(Value, String), ErrorReply> {
    let ExportArgs {
        out,
        format,
        layout,
        scope,
        profile,
    } = a;
    let v = call(
        env,
        "export",
        json!({"out": path_arg(&out)?, "format": format, "layout": layout, "scope": scope, "profile": profile}),
    )?;
    let mut t = format!(
        "exported {} docs in {} folders as {format} to {} ({} bytes)",
        num(&v["docs"]),
        num(&v["folders"]),
        out.display(),
        num(&v["bytes"]),
    );
    if !v["pages"].is_null() {
        t.push_str(&format!(", {} pages", num(&v["pages"])));
    }
    t.push_str(&warning_lines(&v["warnings"]));
    Ok((v, t))
}

pub(crate) fn import(env: &Env, a: ImportArgs) -> Result<(Value, String), ErrorReply> {
    let ImportArgs { dir, view } = a;
    let v = call(env, "import", json!({"dir": path_arg(&dir)?, "view": view}))?;
    let n = v["created"].as_object().map_or(0, |m| m.len());
    let mut t = format!("imported {n} blocks into view {view}");
    let k = v["changes"].as_array().map_or(0, Vec::len);
    if k > 1 {
        t.push_str(&format!(" in {k} changes, one per author"));
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        let doc = w["block"]
            .as_str()
            .map_or("", |id| text(&v["warning_docs"][id]));
        t.push_str(&format!(
            "\nwarning {}: {doc}{}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    if let Some(line) = crate::coverage_cmd::onboarding_line(&v, false) {
        t.push_str(&format!("\n{line}"));
    }
    t.push_str(&brief_text::consequences_text(&v));
    Ok((v, t))
}
