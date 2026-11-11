use crate::brief_text;
use crate::conn::{Env, call, path_arg};
use crate::output::{num, text, warning_lines};
use rdy_daemon::ErrorReply;
use rdy_engine::E_IO;
use serde_json::{Value, json};
use std::io::Write;
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
    /// Read dir as a Markdown source, not a format-3 tree.
    #[arg(long)]
    md: bool,
    /// Preview the Markdown import and write nothing. Needs --md.
    #[arg(long)]
    dry_run: bool,
    /// The source digest of a preview. The import refuses when the source changed. Needs --md.
    #[arg(long, value_name = "DIGEST")]
    expected_source: Option<String>,
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
    lan_warning(&v).map_err(|e| {
        ErrorReply::new(
            E_IO,
            format!("could not print the site's LAN warning: {e}"),
            "check where the standard error goes, then run rdy site again",
        )
    })?;
    let t = format!("{}{}", lines.join("\n"), warning_lines(&v["warnings"]));
    Ok((v, t))
}

/// The structured LAN warning of a `site_url` reply, if any, on stderr: `warning: <message>`,
/// then `fix: <fix>`, one line each, in text and JSON mode alike. A failed write (a closed
/// stderr) is returned, never a panic.
pub(crate) fn lan_warning(v: &Value) -> std::io::Result<()> {
    if v["warning"].is_null() {
        return Ok(());
    }
    let mut err = std::io::stderr().lock();
    writeln!(
        err,
        "warning: {}\nfix: {}",
        text(&v["warning"]["message"]),
        text(&v["warning"]["fix"])
    )?;
    err.flush()
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
    let ImportArgs {
        dir,
        view,
        md,
        dry_run,
        expected_source,
    } = a;
    // the options go out only when set, so a format-3 import sends what it always sent
    let mut args = json!({"dir": path_arg(&dir)?, "view": view});
    if md {
        args["md"] = json!(true);
    }
    if dry_run {
        args["dry_run"] = json!(true);
    }
    if let Some(digest) = expected_source {
        args["expected_source"] = json!(digest);
    }
    let v = call(env, "import", args)?;
    let n = v["created"].as_object().map_or(0, |m| m.len());
    let mut t = if v["dry_run"] == json!(true) {
        format!(
            "previewed {} blocks for view {view}, nothing written",
            num(&v["blocks"])
        )
    } else {
        format!("imported {n} blocks into view {view}")
    };
    let k = v["changes"].as_array().map_or(0, Vec::len);
    if k > 1 {
        t.push_str(&format!(" in {k} changes, one per author"));
    }
    t.push_str(&markdown_lines(&v));
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

/// The lines a Markdown import adds to its text: the source digest, the mapping counts and every
/// `unmapped` row as `path:line kind` (a reported file has line 0, D-2R9EY). Empty for a format-3
/// import, whose reply has no `source_digest`.
fn markdown_lines(v: &Value) -> String {
    let Some(digest) = v["source_digest"].as_str() else {
        return String::new();
    };
    let files = v["files"].as_array().map_or(0, Vec::len);
    let blocks = v["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| num(&f["blocks"]))
        .sum::<u64>();
    let mut t = format!("\nsource digest: {digest}\nmapped: {files} files, {blocks} doc blocks");
    let unmapped = v["unmapped"].as_array().map_or(&[][..], Vec::as_slice);
    t.push_str(&format!("\nunmapped: {}", unmapped.len()));
    for row in unmapped {
        t.push_str(&format!(
            "\n  {}:{} {}",
            text(&row["path"]),
            num(&row["line"]),
            text(&row["kind"])
        ));
    }
    t
}
