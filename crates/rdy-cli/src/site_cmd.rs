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
    /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
}

#[derive(clap::Args)]
pub(crate) struct ExportArgs {
    #[arg(long)]
    out: PathBuf,
    /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
    #[arg(long, default_value = "canon")]
    scope: String,
}

#[derive(clap::Args)]
pub(crate) struct ImportArgs {
    dir: PathBuf,
    #[arg(long)]
    view: String,
}

pub(crate) fn render(env: &Env, a: RenderArgs) -> Result<(Value, String), ErrorReply> {
    let RenderArgs { out, scope } = a;
    let v = call(
        env,
        "render",
        json!({"out": path_arg(&out)?, "scope": scope}),
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

pub(crate) fn export(env: &Env, a: ExportArgs) -> Result<(Value, String), ErrorReply> {
    let ExportArgs { out, scope } = a;
    let v = call(
        env,
        "export",
        json!({"out": path_arg(&out)?, "scope": scope}),
    )?;
    let t = format!(
        "exported {} docs in {} folders, {} blocks, {} blobs{}",
        num(&v["docs"]),
        num(&v["folders"]),
        num(&v["blocks"]),
        num(&v["blobs"]),
        warning_lines(&v["warnings"])
    );
    Ok((v, t))
}

pub(crate) fn import(env: &Env, a: ImportArgs) -> Result<(Value, String), ErrorReply> {
    let ImportArgs { dir, view } = a;
    let v = call(env, "import", json!({"dir": path_arg(&dir)?, "view": view}))?;
    let n = v["created"].as_object().map_or(0, |m| m.len());
    let mut t = format!("imported {n} blocks into view {view}");
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
    t.push_str(&brief_text::consequences_text(&v));
    Ok((v, t))
}
