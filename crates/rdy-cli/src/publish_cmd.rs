//! `rdy publish <tag> --to <remote> --site <site> [--keep-latest <true|false>]` (D-Q0AVZ,
//! D-KQ4CA, D-50JXR; hub i-6ZXW0): the CLI fallback of `rdy_publish`, through the same daemon op
//! and so the same publish coordinator. With `--no-daemon` the op runs in this process, by the
//! direct route every op takes. The hub credentials are the daemon process's (with
//! `--no-daemon`, this process's): after changing `XDG_CONFIG_HOME`, run `rdy serve --stop`.
//!
//! No option names a profile, an audience, a token or an approval: the site selects the profile,
//! the tagged profile gives the audience, and the hub authorizes publication. `--json` prints the
//! publish result unchanged. The text says the upload made a candidate and points to the hub's
//! disclosure review; the inventory and reference lists stay in the JSON.
use crate::conn::{Env, call};
use crate::output::{num, text, warning_lines};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct PublishArgs {
    /// The tag to publish: a version such as v1.2.0.
    tag: String,
    /// The publish remote to upload to, by its name.
    #[arg(long)]
    to: String,
    /// The hub site that receives the candidate. The site selects the publication profile.
    #[arg(long)]
    site: String,
    /// true or false: sent to the hub beside the record. Without it, the hub's default applies.
    #[arg(long, value_name = "KEEP_LATEST", action = clap::ArgAction::Set, value_parser = clap::builder::BoolValueParser::new())]
    keep_latest: Option<bool>,
}

/// The `publish` op's arguments: `keep_latest` only when the flag is given.
pub(crate) fn request(a: &PublishArgs) -> Value {
    let mut args = json!({"tag": a.tag, "to": a.to, "site": a.site});
    if let Some(k) = a.keep_latest {
        args["keep_latest"] = json!(k);
    }
    args
}

/// The text of a publish result: the candidate and its link, the profile (its full id, or
/// `implicit:org`), the audience, the selected block count and the review link; then each
/// settings or credential warning in the shared warning form, then each render warning.
pub(crate) fn result_text(v: &Value) -> String {
    let url = text(&v["url"]);
    let profile = v["record"]["profile"]["id"]
        .as_str()
        .unwrap_or("implicit:org");
    let mut out = format!(
        "candidate {}\n{url}\nprofile: {profile}\naudience: {}\nselected: {} blocks\ndisclosure review: {url}",
        text(&v["id"]),
        text(&v["audience"]),
        num(&v["inventory"]["selection_count"]),
    );
    crate::remote_cmd::push_warnings(&mut out, v);
    out.push_str(&warning_lines(&v["render_warnings"]));
    out
}

/// `rdy publish`: the `publish` op's reply and its text.
pub(crate) fn publish(env: &Env, a: PublishArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "publish", request(&a))?;
    let t = result_text(&v);
    Ok((v, t))
}
