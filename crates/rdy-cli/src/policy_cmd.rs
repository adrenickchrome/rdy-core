//! ap9: `rdy policy init --doc <doc> [--view <view>]` (D-ZWP7Q, O20, n-57ZTN). The command builds
//! one request JSON for the ctl `policy` op; the renderer turns the op's reply into text. `--json`
//! prints the reply as it is.
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum PolicyCmd {
    /// Write a proposed decision and the policy block under the doc, then propose them. A person
    /// approves the proposal like any other.
    Init {
        /// The doc that holds the policy block: a path, a key or an id.
        #[arg(long)]
        doc: String,
        /// The view to write in and propose (default: your own view).
        #[arg(long)]
        view: Option<String>,
    },
}

/// The `policy` op's JSON for `cmd`.
pub fn args_of(cmd: &PolicyCmd) -> Value {
    match cmd {
        PolicyCmd::Init { doc, view } => {
            let mut args = json!({"op": "init", "doc": doc});
            if let Some(v) = view {
                args["view"] = json!(v);
            }
            args
        }
    }
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The text of an `init` reply: the decision and the policy block it wrote, the proposal, and
/// whether a person can approve it (`approve with:`) or what keeps it from that (one line per
/// issue).
pub fn render(v: &Value) -> String {
    let p = &v["proposal"];
    let pref = text(&p["ref"]);
    let mut t = format!(
        "decision {}\npolicy {}\nproposal {pref} ({} changes)",
        text(&v["decision"]),
        text(&v["policy"]),
        p["changes"].as_u64().unwrap_or(0)
    );
    if p["ready"] == true {
        t.push_str(&format!("\nready\napprove with: rdy approve {pref}"));
    } else {
        let issues = p["issues"].as_array().map_or(&[][..], Vec::as_slice);
        t.push_str(&format!("\nnot ready: {} issue(s)", issues.len()));
        for i in issues {
            t.push_str(&format!(
                "\n  {} {}: {} (fix: {})",
                text(&i["code"]),
                i["block"].as_str().unwrap_or("-"),
                text(&i["message"]),
                text(&i["fix"])
            ));
        }
    }
    t
}

pub(crate) fn policy(env: &Env, cmd: PolicyCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "policy", args_of(&cmd))?;
    let t = render(&v);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ap9_init_args_name_the_doc_and_the_view_only_when_given() {
        let init = |view: Option<&str>| PolicyCmd::Init {
            doc: "rules".into(),
            view: view.map(str::to_string),
        };
        assert_eq!(args_of(&init(None)), json!({"op": "init", "doc": "rules"}));
        assert_eq!(
            args_of(&init(Some("w1"))),
            json!({"op": "init", "doc": "rules", "view": "w1"})
        );
    }

    #[test]
    fn ap9_init_text_names_the_records_and_the_next_step() {
        let ready = json!({"decision": "D-AAAAA", "policy": "ap-BBBBB",
            "proposal": {"ref": "P-CCCCC@1", "changes": 2, "ready": true, "issues": []}});
        assert_eq!(
            render(&ready),
            "decision D-AAAAA\npolicy ap-BBBBB\nproposal P-CCCCC@1 (2 changes)\nready\napprove with: rdy approve P-CCCCC@1"
        );
        let not = json!({"decision": "D-AAAAA", "policy": "ap-BBBBB",
            "proposal": {"ref": "P-CCCCC@1", "changes": 2, "ready": false,
                "issues": [{"code": "E_VALIDATION", "block": null, "message": "m", "fix": "f"}]}});
        assert_eq!(
            render(&not),
            "decision D-AAAAA\npolicy ap-BBBBB\nproposal P-CCCCC@1 (2 changes)\nnot ready: 1 issue(s)\n  E_VALIDATION -: m (fix: f)"
        );
    }
}
