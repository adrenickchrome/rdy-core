//! `rdy redact`, the CLI twin of `rdy_redact` (D-KQ4CA: the CLI keeps parity with the tools).
//! It sends the shared daemon op `redact` with that op's keys and defaults, and prints the shared
//! formatter's text for the result (`report_text`); `--json` prints the op's reply. A dry run
//! changes nothing; a request erases nothing. There is no switch that approves: a person
//! approves the proposal that carries the request (D-CC90E).
use crate::conn::{Env, call};
use rdy_daemon::ErrorReply;
use rdy_engine::redaction::presentation::DEFAULT_TOKENS;
use rdy_engine::redaction::{RedactResult, report_text};
use rdy_engine::{E_INTERNAL, RdyError};
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct RedactArgs {
    /// What to remove: a block id (every version), id@full-version-hash, or
    /// blob:full-manifest-hash.
    #[arg(value_name = "TARGET")]
    targets: Vec<String>,
    /// Why: secret, personal, legal or retraction.
    #[arg(long, value_name = "CLASS")]
    reason_class: String,
    /// The reason in your words (1 to 4096 UTF-8 bytes).
    #[arg(long)]
    reason: String,
    /// Print the report and record nothing.
    #[arg(long)]
    dry_run: bool,
    /// The view the request goes in (default: the session's, `work` for the human). The
    /// targets are found in the whole project.
    #[arg(long)]
    view: Option<String>,
    /// Print the report within this many tokens (256 to 20000, default 4000). A smaller budget
    /// leaves out the rows, never the warnings or the request.
    #[arg(long)]
    max_tokens: Option<usize>,
}

/// The args of the `redact` op for `a`: every key of the op, the omitted ones null.
pub(crate) fn request(a: &RedactArgs) -> Value {
    json!({
        "targets": a.targets,
        "reason_class": a.reason_class,
        "reason": a.reason,
        "dry_run": a.dry_run,
        "view": a.view,
        "max_tokens": a.max_tokens,
    })
}

pub(crate) fn redact(env: &Env, a: RedactArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "redact", request(&a))?;
    let t = text_of(&v, a.max_tokens)?;
    Ok((v, t))
}

/// The result keys of a redact reply: the mutation envelope of a request adds its own.
const RESULT_KEYS: [&str; 7] = [
    "dry_run",
    "reason_class",
    "reason",
    "change",
    "redaction",
    "report",
    "warnings",
];

/// The shared formatter's text for the result `v` holds, within `max_tokens` (the default when
/// none is given), as the op computed it.
pub(crate) fn text_of(v: &Value, max_tokens: Option<usize>) -> Result<String, ErrorReply> {
    let result: Value = RESULT_KEYS
        .iter()
        .map(|k| (k.to_string(), v[*k].clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let result: RedactResult = serde_json::from_value(result).map_err(|e| {
        ErrorReply::from(RdyError::new(
            E_INTERNAL,
            format!("the redact reply holds no redaction result: {e}"),
            "run the command again with --json to read the reply",
        ))
    })?;
    Ok(report_text(&result, max_tokens.unwrap_or(DEFAULT_TOKENS))?.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::redaction::{RedactInput, dry_run, report_text};
    use rdy_engine::{Rdy, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    /// rdx13: the text `rdy redact` prints is the shared formatter's for the reply's result at
    /// the budget asked (4000 tokens when none is), whatever else the reply's envelope holds.
    #[test]
    fn rdx13_redact_text_is_the_shared_formatters() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let ops = serde_json::from_value(json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]))
        .unwrap();
        let w = r
            .write(
                &Session::human("human:t".into(), Role::Writer),
                WriteRequest {
                    view: "work".into(),
                    ops,
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        let input = RedactInput {
            targets: vec![w.created["$p"].clone()],
            reason_class: "secret".into(),
            reason: "a leak".into(),
            dry_run: true,
        };
        let res = dry_run(&r.read(), &input).unwrap();
        r.close().unwrap();
        let mut v = serde_json::to_value(&res).unwrap();
        v["consequences"] = json!([]);
        v["propose_due"] = json!(true);
        for max in [None, Some(400), Some(20_000)] {
            let want = report_text(&res, max.unwrap_or(4000)).unwrap();
            v["text"] = json!(want.text);
            assert_eq!(text_of(&v, max).unwrap(), want.text, "{max:?}");
        }
        // a reply that holds no result is an error, never a text made up here
        assert!(text_of(&json!({"text": "x"}), None).is_err());
    }
}
