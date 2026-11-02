//! `rdy compact [--dry-run]` (D-4YRQ4, D-KQ4CA): the CLI twin of `rdy_compact`. It calls the
//! `compact` op, so the report is the engine's shared `CompactReport`; JSON prints it whole.
use crate::conn::{Env, call};
use crate::output::{num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct CompactArgs {
    /// Report the bytes without punching holes: the same fields, and punched bytes 0.
    #[arg(long)]
    dry_run: bool,
}

/// One `<name>: <bytes> bytes, <removed_bytes> removed` line per kind of the report `v`, in the
/// ascending numeric order the report gives.
pub(crate) fn kind_lines(v: &Value) -> Vec<String> {
    v["by_kind"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| {
            format!(
                "{}: {} bytes, {} removed",
                text(&k["name"]),
                num(&k["bytes"]),
                num(&k["removed_bytes"])
            )
        })
        .collect()
}

/// `log bytes:` and `removed bytes:`, then the kind lines: what `rdy fsck` appends.
pub(crate) fn byte_lines(v: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("log bytes: {}", num(&v["log_bytes"])),
        format!("removed bytes: {}", num(&v["removed_bytes"])),
    ];
    lines.extend(kind_lines(v));
    lines
}

/// The text of a compact report: four byte lines, the kind lines, then each note verbatim.
pub(crate) fn report_text(v: &Value) -> String {
    let mut lines = vec![
        format!("log bytes: {}", num(&v["log_bytes"])),
        format!("removed bytes: {}", num(&v["removed_bytes"])),
        format!("punched bytes: {}", num(&v["punched_bytes"])),
        format!("on-disk bytes: {}", num(&v["on_disk_bytes"])),
    ];
    lines.extend(kind_lines(v));
    lines.extend(
        v["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| text(n).to_string()),
    );
    lines.join("\n")
}

pub(crate) fn compact(env: &Env, a: CompactArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "compact", json!({"dry_run": a.dry_run}))?;
    let t = report_text(&v);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mh7: the four lines in order, a line per kind in the report's order, then each note
    /// verbatim; an fsck tail has the first two lines and the kinds.
    #[test]
    fn mh7_the_report_text_has_its_lines_in_order() {
        let notes = [
            "hole punching is not supported for 2 removed spans; they keep their disk space, and their content stays removed",
            "hole punching failed for 1 removed spans; they keep their disk space, and their content stays removed",
            "disk allocation could not be read after punching; on_disk_bytes is the count before this call",
        ];
        let v = json!({
            "log_bytes": 900, "removed_bytes": 40, "punched_bytes": 0, "on_disk_bytes": 4096,
            "by_kind": [
                {"kind": 1, "name": "block", "bytes": 300, "removed_bytes": 40},
                {"kind": 14, "name": "batch_commit", "bytes": 600, "removed_bytes": 0}
            ],
            "notes": notes,
        });
        let want = [
            "log bytes: 900",
            "removed bytes: 40",
            "punched bytes: 0",
            "on-disk bytes: 4096",
            "block: 300 bytes, 40 removed",
            "batch_commit: 600 bytes, 0 removed",
        ]
        .into_iter()
        .chain(notes)
        .collect::<Vec<_>>()
        .join("\n");
        assert_eq!(report_text(&v), want);
        assert_eq!(
            byte_lines(&v),
            [
                "log bytes: 900",
                "removed bytes: 40",
                "block: 300 bytes, 40 removed",
                "batch_commit: 600 bytes, 0 removed"
            ]
        );
    }
}
